// Copyright (c) 2022-2025 Alex Chi Z
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

use std::ops::Bound;

use anyhow::{Result, bail};
use bytes::Bytes;

use crate::iterators::StorageIterator;
use crate::iterators::concat_iterator::SstConcatIterator;
use crate::iterators::merge_iterator::MergeIterator;
use crate::iterators::two_merge_iterator::TwoMergeIterator;
use crate::mem_table::MemTableIterator;
use crate::table::SsTableIterator;

/// Read the nesting as merge(merge(memtables, L0), levels/tiers).
/// Each TwoMergeIterator prefers its left input on equal keys; each MergeIterator
/// prefers its earlier input. Together they encode the storage source precedence.
type LsmIteratorInner = TwoMergeIterator<
    TwoMergeIterator<MergeIterator<MemTableIterator>, MergeIterator<SsTableIterator>>,
    MergeIterator<SstConcatIterator>,
>;

pub struct LsmIterator {
    inner: LsmIteratorInner,
    end_bound: Bound<Bytes>,
    // Logical scan validity can be false while inner still has keys: those keys
    // might all lie beyond this scan's upper bound.
    is_valid: bool,
}

impl LsmIterator {
    pub(crate) fn new(iter: LsmIteratorInner, end_bound: Bound<Bytes>) -> Result<Self> {
        let mut iter = Self {
            is_valid: iter.is_valid(),
            inner: iter,
            end_bound,
        };
        // A lower-bound seek may already land past the upper bound, even before
        // the caller's first next(). Validate the initial position as well.
        // Example: scan [b,b], SST keys [a,c]. The seek lands on c; return an
        // invalid scan immediately rather than exposing c until next() is called.
        iter.check_end_bound();
        iter.move_to_non_delete()?;
        Ok(iter)
    }

    fn check_end_bound(&mut self) {
        if !self.is_valid {
            return;
        }
        // SST cursors know where to start, but have no upper scan bound. Because
        // the merged stream is sorted, crossing this bound ends the whole scan.
        match self.end_bound.as_ref() {
            Bound::Unbounded => {}
            Bound::Included(key) => self.is_valid = self.inner.key().raw_ref() <= key.as_ref(),
            Bound::Excluded(key) => self.is_valid = self.inner.key().raw_ref() < key.as_ref(),
        }
    }

    fn next_inner(&mut self) -> Result<()> {
        // Advance by one distinct merged key, which might still be a tombstone.
        // Do this helper's exhaustion/bound checks on EVERY raw step, including
        // the extra steps taken by move_to_non_delete().
        self.inner.next()?;
        if !self.inner.is_valid() {
            self.is_valid = false;
            return Ok(());
        }
        self.check_end_bound();
        Ok(())
    }

    fn move_to_non_delete(&mut self) -> Result<()> {
        // Filter only AFTER merging has chosen the newest entry. Advancing the
        // merge also consumes older copies, so a deleted key cannot reappear.
        // Example raw stream: [b:delete, c:delete, d:4]. Keep stepping until d,
        // unless the upper bound ends the scan first. This needs a loop, not if.
        while self.is_valid() && self.inner.value().is_empty() {
            self.next_inner()?;
        }
        Ok(())
    }
}

impl StorageIterator for LsmIterator {
    type KeyType<'a> = &'a [u8];

    fn is_valid(&self) -> bool {
        self.is_valid
    }

    fn key(&self) -> &[u8] {
        self.inner.key().raw_ref()
    }

    fn value(&self) -> &[u8] {
        self.inner.value()
    }

    fn next(&mut self) -> Result<()> {
        // One user-visible step may advance several raw entries: consume the
        // current key, then skip any consecutive deleted keys that follow it.
        self.next_inner()?;
        self.move_to_non_delete()?;
        Ok(())
    }

    fn num_active_iterators(&self) -> usize {
        self.inner.num_active_iterators()
    }
}

/// A wrapper around existing iterator, will prevent users from calling `next` when the iterator is
/// invalid. If an iterator is already invalid, `next` does not do anything. If `next` returns an error,
/// `is_valid` should return false, and `next` should always return an error.
pub struct FusedIterator<I: StorageIterator> {
    iter: I,
    has_errored: bool,
}

impl<I: StorageIterator> FusedIterator<I> {
    pub fn new(iter: I) -> Self {
        Self {
            iter,
            has_errored: false,
        }
    }
}

impl<I: StorageIterator> StorageIterator for FusedIterator<I> {
    type KeyType<'a>
        = I::KeyType<'a>
    where
        Self: 'a;

    fn is_valid(&self) -> bool {
        // Even if a child still reports a position, an earlier error taints it.
        !self.has_errored && self.iter.is_valid()
    }

    fn key(&self) -> Self::KeyType<'_> {
        if !self.is_valid() {
            panic!("invalid access to the underlying iterator");
        }
        self.iter.key()
    }

    fn value(&self) -> &[u8] {
        if !self.is_valid() {
            panic!("invalid access to the underlying iterator");
        }
        self.iter.value()
    }

    fn next(&mut self) -> Result<()> {
        // An I/O failure can leave children partially advanced. Make the error
        // permanent rather than exposing a possibly inconsistent merged result.
        if self.has_errored {
            bail!("the iterator is tainted");
        }
        // Normal exhaustion is different from failure: after reaching the end,
        // repeated next() calls return Ok(()) without touching the inner cursor.
        if self.iter.is_valid()
            && let Err(e) = self.iter.next()
        {
            self.has_errored = true;
            return Err(e);
        }
        Ok(())
    }

    fn num_active_iterators(&self) -> usize {
        self.iter.num_active_iterators()
    }
}
