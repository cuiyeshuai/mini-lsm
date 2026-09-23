// Copyright (c) 2022-2026 Alex Chi Z
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    collections::HashSet,
    ops::Bound,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Result, bail};
use bytes::Bytes;
use crossbeam_skiplist::{SkipMap, map::Entry};
use ouroboros::self_referencing;
use parking_lot::Mutex;

use crate::{
    iterators::{StorageIterator, two_merge_iterator::TwoMergeIterator},
    lsm_iterator::{FusedIterator, LsmIterator},
    lsm_storage::{LsmStorageInner, WriteBatchRecord},
    mem_table::map_bound,
    mvcc::CommittedTxnData,
};

/// Use one foreground thread per transaction, including its iterators. The atomic
/// committed flag prevents a second commit; it does not make concurrent workspace
/// edits and commit one atomic operation. Separate transactions can run concurrently.
pub struct Transaction {
    pub(crate) read_ts: u64,
    pub(crate) inner: Arc<LsmStorageInner>,
    pub(crate) local_storage: Arc<SkipMap<Bytes, Bytes>>,
    pub(crate) committed: Arc<AtomicBool>,
    /// Write set and read set
    pub(crate) key_hashes: Option<Mutex<(HashSet<u32>, HashSet<u32>)>>,
}

impl Transaction {
    pub fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        // Record the dependency even for a missing key. Then read your own local
        // writes first; only a local MISS falls through to the shared snapshot.
        // A local tombstone is an authoritative result, not a reason to fall through.
        if self.committed.load(Ordering::SeqCst) {
            panic!("cannot operate on committed txn!");
        }
        if let Some(guard) = &self.key_hashes {
            let mut guard = guard.lock();
            let (_, read_set) = &mut *guard;
            read_set.insert(farmhash::hash32(key));
        } // Release key_hashes before any shared-engine read or further iteration.
        if let Some(entry) = self.local_storage.get(key) {
            if entry.value().is_empty() {
                return Ok(None);
            } else {
                return Ok(Some(entry.value().clone()));
            }
        }
        self.inner.get_with_ts(key, self.read_ts)
    }

    pub fn scan(self: &Arc<Self>, lower: Bound<&[u8]>, upper: Bound<&[u8]>) -> Result<TxnIterator> {
        // Merge two user-key streams: private workspace first, snapshot second.
        // Local updates win equal keys, including deletes. TxnIterator filters
        // tombstones afterward and retains this transaction through an Arc.
        if self.committed.load(Ordering::SeqCst) {
            panic!("cannot operate on committed txn!");
        }
        let mut local_iter = TxnLocalIteratorBuilder {
            map: self.local_storage.clone(),
            iter_builder: |map| map.range((map_bound(lower), map_bound(upper))),
            item: (Bytes::new(), Bytes::new()),
        }
        .build();
        local_iter.next()?;

        TxnIterator::create(
            self.clone(),
            TwoMergeIterator::create(
                local_iter,
                self.inner.scan_with_ts(lower, upper, self.read_ts)?,
            )?,
        )
    }

    pub fn put(&self, key: &[u8], value: &[u8]) {
        // Stage the latest value in a timestamp-free private map. No shared
        // memtable or WAL write happens until commit; repeated puts collapse here.
        if self.committed.load(Ordering::SeqCst) {
            panic!("cannot operate on committed txn!");
        }
        self.local_storage
            .insert(Bytes::copy_from_slice(key), Bytes::copy_from_slice(value));
        if let Some(key_hashes) = &self.key_hashes {
            let mut key_hashes = key_hashes.lock();
            let (write_hashes, _) = &mut *key_hashes;
            write_hashes.insert(farmhash::hash32(key));
        } // Release key_hashes; no mutex is retained between transaction operations.
    }

    pub fn delete(&self, key: &[u8]) {
        // Keep a local tombstone so both get and scan hide the shared snapshot's
        // older entry. Removing the local map entry would undo that override.
        if self.committed.load(Ordering::SeqCst) {
            panic!("cannot operate on committed txn!");
        }
        self.local_storage
            .insert(Bytes::copy_from_slice(key), Bytes::new());
        if let Some(key_hashes) = &self.key_hashes {
            let mut key_hashes = key_hashes.lock();
            let (write_hashes, _) = &mut *key_hashes;
            write_hashes.insert(farmhash::hash32(key));
        } // Release key_hashes; no mutex is retained between transaction operations.
    }

    pub fn commit(&self) -> Result<()> {
        // One-shot state transition, then validate -> publish -> record history
        // under commit_lock. A failed validation consumes this transaction but
        // publishes none of its staged entries. The course assumes one user thread
        // operates on a transaction; this flag is not a lock around its workspace.
        self.committed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .expect("cannot operate on committed txn!");
        // Acquire until function return, including empty commits and `?`/bail exits.
        // This named guard also remains held while write_batch_inner takes write_lock.
        let _commit_lock = self.inner.mvcc().commit_lock.lock();
        let serializability_check;
        if let Some(guard) = &self.key_hashes {
            // During validation: key_hashes -> committed_txns (if there are writes).
            let guard = guard.lock();
            let (write_set, read_set) = &*guard;
            println!(
                "commit txn: write_set: {:?}, read_set: {:?}",
                write_set, read_set
            );
            if !write_set.is_empty() {
                let committed_txns = self.inner.mvcc().committed_txns.lock();
                for (_, txn_data) in committed_txns.range((self.read_ts + 1)..) {
                    // Check our READS against writes committed after our snapshot.
                    // T1 reads b/writes a; T2 reads a/writes b. Once T1 commits,
                    // T2's read(a) intersects T1's write(a), so reject T2.
                    for key_hash in read_set {
                        if txn_data.key_hashes.contains(key_hash) {
                            bail!("serializable check failed");
                        }
                    }
                }
            }
            // committed_txns released at the inner if-block's end; key_hashes
            // releases at the end of this outer branch, before batch publication.
            serializability_check = true;
        } else {
            serializability_check = false;
        }
        let batch = self
            .local_storage
            .iter()
            .map(|entry| {
                if entry.value().is_empty() {
                    WriteBatchRecord::Del(entry.key().clone())
                } else {
                    WriteBatchRecord::Put(entry.key().clone(), entry.value().clone())
                }
            })
            .collect::<Vec<_>>();
        if batch.is_empty() {
            // Read-only commit needs no new timestamp, WAL record, or history entry.
            return Ok(());
        }
        // write_batch_inner releases write_lock before returning. A post-publication
        // freeze error can return here before history registration; errors do not
        // generally mean "nothing committed" or promise safe continued validation.
        let ts = self.inner.write_batch_inner(&batch)?;
        if serializability_check {
            // Registration takes committed_txns -> key_hashes, the reverse of
            // validation. commit_lock excludes other commits during both phases;
            // this is NOT a universal lock order usable by arbitrary new callers.
            let mut committed_txns = self.inner.mvcc().committed_txns.lock();
            let mut key_hashes = self.key_hashes.as_ref().unwrap().lock();
            let (write_set, _) = &mut *key_hashes;

            let old_data = committed_txns.insert(
                ts,
                CommittedTxnData {
                    key_hashes: std::mem::take(write_set),
                    read_ts: self.read_ts,
                    commit_ts: ts,
                },
            );
            assert!(old_data.is_none());

            // watermark() briefly acquires ts while both guards above are held.
            // Remove old conflict metadata; both guards release at this if's end.
            let watermark = self.inner.mvcc().watermark();
            while let Some(entry) = committed_txns.first_entry() {
                if *entry.key() < watermark {
                    entry.remove();
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // A TxnIterator holds an Arc<Transaction>, so dropping the user's handle
        // alone cannot release the snapshot while that scan still needs it.
        // Acquire ts to decrement the reader count; release on returning from Drop.
        // commit() does not unregister: the final Arc drop does.
        self.inner.mvcc().ts.lock().1.remove_reader(self.read_ts)
    }
}

type SkipMapRangeIter<'a> =
    crossbeam_skiplist::map::Range<'a, Bytes, (Bound<Bytes>, Bound<Bytes>), Bytes, Bytes>;

#[self_referencing]
pub struct TxnLocalIterator {
    /// Stores a reference to the skipmap.
    map: Arc<SkipMap<Bytes, Bytes>>,
    /// Stores a skipmap iterator that refers to the lifetime of `TxnLocalIterator` itself.
    #[borrows(map)]
    #[not_covariant]
    iter: SkipMapRangeIter<'this>,
    /// Stores the current key-value pair.
    item: (Bytes, Bytes),
}

impl TxnLocalIterator {
    fn entry_to_item(entry: Option<Entry<'_, Bytes, Bytes>>) -> (Bytes, Bytes) {
        entry
            .map(|x| (x.key().clone(), x.value().clone()))
            .unwrap_or_else(|| (Bytes::new(), Bytes::new()))
    }
}

impl StorageIterator for TxnLocalIterator {
    type KeyType<'a> = &'a [u8];

    fn value(&self) -> &[u8] {
        &self.borrow_item().1[..]
    }

    fn key(&self) -> &[u8] {
        &self.borrow_item().0[..]
    }

    fn is_valid(&self) -> bool {
        !self.borrow_item().0.is_empty()
    }

    fn next(&mut self) -> Result<()> {
        let entry = self.with_iter_mut(|iter| TxnLocalIterator::entry_to_item(iter.next()));
        self.with_mut(|x| *x.item = entry);
        Ok(())
    }
}

pub struct TxnIterator {
    txn: Arc<Transaction>,
    iter: TwoMergeIterator<TxnLocalIterator, FusedIterator<LsmIterator>>,
}

impl TxnIterator {
    pub fn create(
        txn: Arc<Transaction>,
        iter: TwoMergeIterator<TxnLocalIterator, FusedIterator<LsmIterator>>,
    ) -> Result<Self> {
        let mut iter = Self { txn, iter };
        iter.skip_deletes()?;
        if iter.is_valid() {
            iter.add_to_read_set(iter.key());
        }
        Ok(iter)
    }

    fn skip_deletes(&mut self) -> Result<()> {
        while self.iter.is_valid() && self.iter.value().is_empty() {
            self.iter.next()?;
        }
        Ok(())
    }

    fn add_to_read_set(&self, key: &[u8]) {
        // Only returned keys are tracked. An empty scan contributes no key hashes,
        // so inserts into gaps (phantoms) are NOT detected by this implementation.
        if let Some(guard) = &self.txn.key_hashes {
            let mut guard = guard.lock();
            let (_, read_set) = &mut *guard;
            read_set.insert(farmhash::hash32(key));
        } // Release key_hashes before any shared-engine read or further iteration.
    }
}

impl StorageIterator for TxnIterator {
    type KeyType<'a>
        = &'a [u8]
    where
        Self: 'a;

    fn value(&self) -> &[u8] {
        self.iter.value()
    }

    fn key(&self) -> Self::KeyType<'_> {
        self.iter.key()
    }

    fn is_valid(&self) -> bool {
        self.iter.is_valid()
    }

    fn next(&mut self) -> Result<()> {
        self.iter.next()?;
        self.skip_deletes()?;
        if self.is_valid() {
            self.add_to_read_set(self.key());
        }
        Ok(())
    }

    fn num_active_iterators(&self) -> usize {
        self.iter.num_active_iterators()
    }
}
