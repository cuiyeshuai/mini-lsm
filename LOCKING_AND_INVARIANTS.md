# Locks, ownership, and the guarantees behind the code

Read this alongside the [course roadmap](READING_ROADMAP.md). It fills in the
cross-cutting details behind the chapter tasks: who owns data, which lock protects
which operation, when each guard releases, and what each helper actually promises.
Links below open the annotated implementation. Week 1–2 uses `mini-lsm/`; Week 3
uses `mini-lsm-mvcc/`.

Start with [guard lifetimes](#1-how-to-recognize-acquisition-and-release), then
[reads](#3-reads-pin-objects-without-holding-the-layout-lock). Return to each later
section when its operation appears in the weekly roadmap.

## 1. How to recognize acquisition and release

`parking_lot::Mutex::lock()`, `RwLock::read()`, and `RwLock::write()` acquire a lock
and return a **guard**. The guard's lifetime is the lock's lifetime. A read guard
allows other readers; a write guard excludes both readers and other writers.

```rust
let snapshot = {
    let guard = self.state.read(); // Acquire the layout read lock.
    Arc::clone(&guard)             // Keep the layout alive independently.
};                                // Drop guard: release the read lock.
// snapshot is an Arc, NOT a guard. It does not keep the lock acquired.
```

You will see four distinct patterns:

| Pattern in this code | When the lock releases |
| --- | --- |
| `let guard = lock.lock();` | At the end of the guard's scope, including an early return |
| `drop(guard);` | At that explicit statement |
| `let _guard = lock.lock();` | Also at scope exit: an underscore-prefixed **name** still holds a guard |
| `let value = self.ts.lock().0;` | The temporary guard releases at that statement's end, after copying the value |

Do not confuse `_guard` with the discard pattern `_`. Nor should you assume every
temporary releases before a nested call: in `self.state.read().memtable.sync_wal()`
the read guard remains alive throughout `sync_wal()`. Follow the actual expression
and enclosing scope. An unused named guard is not released at its last textual use.

`?`, `bail!`, and ordinary early returns drop live local guards as they leave their
scopes. That releases locks; it **does not undo** earlier map updates, file writes,
or published state. Panic unwinding also drops guards; process abort is different.

## 2. What each lock protects

Start at [LsmStorageInner](mini-lsm/src/lsm_storage.rs#L198), its
[MVCC counterpart](mini-lsm-mvcc/src/lsm_storage.rs#L202), and
[LsmMvccInner](mini-lsm-mvcc/src/mvcc.rs#L41).

| Lock | Protected operation/data | Lifetime to look for |
| --- | --- | --- |
| `state: RwLock<Arc<LsmStorageState>>` | Accessing or replacing the current source-layout pointer; writes also retain a read guard to prevent rotation during insertion | Short layout clone/swap; longer during memtable writes and `sync` |
| `state_lock: Mutex<()>` | Serializing structural sequences: freeze, flush, compaction installation, and WAL sync against rotation | Can span file I/O; independent of the shorter state guard |
| WAL `file` mutex | Append and sync of one WAL's `BufWriter<File>` | Through the individual WAL call |
| Manifest `file` mutex | Encoding, appending, and syncing one manifest record | Through `add_record_when_init` |
| MVCC `write_lock` | Serializing a batch's timestamp allocation, insertion, publication, and subsequent freeze attempt | All of nonempty `write_batch_inner` after validation |
| MVCC `commit_lock` | Serializing transaction validation, publication, and conflict-history registration | `Transaction::commit`, including cleanup and error exits |
| MVCC `ts` mutex | Published timestamp and active-reader counts together | Short clock/registry operations; never a whole transaction |
| MVCC `committed_txns` mutex | Recorded write sets indexed by commit timestamp | Validation or registration/cleanup blocks |
| Transaction `key_hashes` mutex | This transaction's read/write hash sets | Individual tracking calls and commit's nested blocks |
| `compaction_filters` mutex | Registered filter list | Append a policy, or clone the list for a rewrite |
| `compaction_thread` / `flush_thread` mutexes | Taking and joining worker handles during close | Named guards remain held until `close` returns |

An `Arc` provides **shared ownership**, not mutual exclusion or immutable contents.
The state is a layout of collections containing Arcs. `Arc::clone(&guard)` retains
the same layout object; `guard.as_ref().clone()` copies its collections while sharing
the underlying memtables/SSTs. The latter prepares a replacement layout.

Skipmaps provide concurrent entry operations without an enclosing memtable mutex.
The block cache has its own library-managed synchronization. Therefore “no state
lock during reads” must not be read as “all code reached here is lock-free.”

## 3. Reads pin objects without holding the layout lock

Read [get](mini-lsm/src/lsm_storage.rs#L536) and
[scan](mini-lsm/src/lsm_storage.rs#L842), then their Week 3 equivalents
[get_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L547) and
[scan_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L878).

1. Acquire `state.read()` inside the `snapshot` block.
2. Clone the layout Arc, then release the read guard at the block's closing brace.
3. Probe memory or build/advance SST cursors. These operations run without that
   state guard and without acquiring `state_lock`.
4. Cursors retain the maps/tables/blocks they need through owned references. This
   keeps their objects alive even when another thread replaces the current layout.

A Week 1–2 source-layout snapshot does **not** freeze values in its shared current
memtable. It is not a point-in-time database snapshot. In Week 3, visibility instead
comes from a registered `read_ts` and retained versions. The timestamp registration
is separate from the state-layout read lock; see [transaction lifetime](#7-transaction-lifetime-and-commit).

For a scan, releasing the state guard is not deferred until the caller finishes
iteration. The iterator tree itself does not acquire engine locks to compare keys;
its children may perform disk/cache work, and `TxnIterator` briefly locks its read
set when tracking a visited key.

## 4. Writes and freeze: why dropping the read guard matters

### Week 1–2: one record at a time

Read [write_batch](mini-lsm/src/lsm_storage.rs#L630) →
[MemTable::put](mini-lsm/src/mem_table.rs#L115) →
[try_freeze](mini-lsm/src/lsm_storage.rs#L680).

For each record, acquire `state.read()`, mutate the current skipmap, optionally
append to its WAL, and read the estimated size. The WAL call acquires/releases its
own file mutex while the state read guard remains held. Exit the inner block to
release the state guard **before** calling `try_freeze`.

This guard stops rotation from redirecting the write halfway through; it does not
serialize concurrent writes, since they can all hold read guards. This solution
updates memory before appending the WAL. A WAL error therefore does not roll back
memory. Two concurrent writers can also order their map updates and WAL appends
differently: the file mutex alone does not make that pair of operations atomic.
The Week 2 batch API supplies neither atomic batch visibility nor atomic replay.

### Week 3: one timestamp for the entire batch

Read [write_batch_inner](mini-lsm-mvcc/src/lsm_storage.rs#L629) →
[MemTable::put_batch](mini-lsm-mvcc/src/mem_table.rs#L165) →
[Wal::put_batch](mini-lsm-mvcc/src/wal.rs#L130).

1. Empty batch: return the current timestamp without taking `write_lock`.
2. Validate encoded sizes; acquire `write_lock` for the remaining function.
3. Briefly acquire/release `ts` to read the published clock, then allocate `ts + 1`.
4. Acquire `state.read()` to pin the writable memtable. With WAL enabled, append
   one frame under the WAL file mutex, release that mutex, then insert all entries.
5. Release the state read guard at the inner block's end.
6. Briefly acquire/release `ts` to publish the batch timestamp.
7. Attempt freeze maintenance, still holding `write_lock`. Release `write_lock`
   when the function returns, including an error return.

A new snapshot cannot select a half-inserted batch: it captures the old clock until
step 6. Raw memtable cursors do not provide that guarantee themselves. WAL append
is conditional on WAL being enabled and does not itself call `sync_all`.

Publication precedes fallible freeze maintenance. Consequently a freeze error can
return `Err` **after the batch is visible**. “Atomic visibility” is not the claim
that every error means nothing happened.

### Freeze in both solutions

Read [try_freeze](mini-lsm/src/lsm_storage.rs#L680) →
[force_freeze_memtable](mini-lsm/src/lsm_storage.rs#L743) →
[freeze_memtable_with_memtable](mini-lsm/src/lsm_storage.rs#L720).
The [Week 3 path](mini-lsm-mvcc/src/lsm_storage.rs#L725) follows the same structure.

1. If the initial size estimate reaches the target, acquire `state_lock`.
2. Acquire `state.read()` and recheck the current map's size: another writer might
   already have frozen the map that produced the original estimate.
3. If rotation is still needed, explicitly `drop(guard)` before entering the helper
   that will acquire `state.write()`. Retaining that read guard would block our own
   attempt to obtain exclusive access.
4. `force_freeze_memtable` **borrows** the existing structural guard. It creates
   the replacement and records `NewMemtable`; it does not acquire `state_lock` anew.
5. `freeze_memtable_with_memtable` acquires `state.write()`, clones the layout's
   collections, moves the old current map to the front of immutables, and publishes
   the new layout. It explicitly drops the write guard before syncing the old WAL.
6. The caller's structural guard remains held until the outer `if` block ends.
   If the size recheck fails, both guards simply release at scope exit.

Freeze changes `current=A, immutable=[B]` to `current=C, immutable=[A,B]`.
It constructs no SST. If the old WAL sync fails, the layout swap has already happened.

## 5. Flush and compaction have different lock lifetimes

### Flush

Read [force_flush_next_imm_memtable](mini-lsm/src/lsm_storage.rs#L773) or its
[Week 3 version](mini-lsm-mvcc/src/lsm_storage.rs#L811).

Acquire `state_lock` at entry and retain it **through the entire function**. Inside
that interval:

1. Briefly acquire `state.read()`, select the oldest immutable map, clone its Arc,
   and release the read guard. With no immutable map, return and release both guards.
2. Build and sync the SST, then sync the directory. No state RwLock guard spans
   this work, but `state_lock` is still held.
3. Acquire `state.write()` to remove that immutable map and install the SST in
   L0, or a new front tier. Release the write guard at the inner block's end.
4. Append/sync `Flush(id)` under the manifest file mutex; release that file mutex.
   Remove the old WAL if enabled, sync the directory, then return and release
   `state_lock`.

Thus readers can clone the layout during SST construction, while another structural
operation must wait. A foreground write can update the current map, but a write
that then needs to freeze may wait for this flush's structural mutex.

### Compaction

Read [trigger_compaction](mini-lsm/src/compact.rs#L352) →
[compact](mini-lsm/src/compact.rs#L190); compare
[Week 3 trigger_compaction](mini-lsm-mvcc/src/compact.rs#L407).

1. Acquire/release `state.read()` to retain a planning layout. The controller chooses
   exact input ids; it does not acquire engine locks or edit the live layout.
2. `compact` briefly acquires/releases another state read guard to retain input
   table objects, then builds outputs without holding either `state_lock` or a state
   RwLock guard. MVCC watermark/filter capture also uses only short mutex scopes.
3. Acquire `state_lock` for installation. Clone the **current** layout under a
   temporary state read guard, which releases at its statement's semicolon.
4. Apply the planned replacement to that current layout, preserving unrelated new
   files. Acquire `state.write()` to publish, then explicitly `drop(state)`.
5. Still holding `state_lock`, sync the directory and append/sync the manifest edit.
   Release `state_lock` at the installation block's end.
6. Delete obsolete input file names, then sync the directory.

[force_full_compaction](mini-lsm/src/compact.rs#L285) similarly retains the structural
mutex only for installation/persistence; its temporary state write guard releases
at the assignment's semicolon. It requires `NoCompaction` mode. The normal design
has one compaction worker; installation locking does not make arbitrarily concurrent
manual compactions safe. Input-selection assumptions still matter.

Old readers retain their opened table objects while file names are removed. For this
Unix-oriented implementation, unlinking a name does not close those open file handles.
Neither installation path rolls back an already published layout on a later I/O error.

## 6. Persistence, workers, and shutdown

Read [Manifest::add_record](mini-lsm/src/manifest.rs#L131) →
[add_record_when_init](mini-lsm/src/manifest.rs#L142). The first receives a borrowed
structural guard and the second acquires the manifest file mutex. File locking covers
serialization, append, and `sync_all`, releasing on return. The borrowed guard remains
owned by the caller. Its Rust type does not prove which mutex created it; callers
must supply this engine's structural guard. Initialization uses the lower-level
method before the engine is shared with workers.

Read [Wal::put](mini-lsm/src/wal.rs#L115), [Wal::sync](mini-lsm/src/wal.rs#L148), and
[LsmStorageInner::sync](mini-lsm/src/lsm_storage.rs#L519). Appending uses `BufWriter`;
it may write bytes to the OS without making them durable. WAL `sync` holds the file
mutex through both buffer flush and `sync_all`. Engine `sync` holds
`state_lock → state.read() → WAL file mutex`; guards release as calls return. It
prevents rotation during the sync but does not take MVCC `write_lock` or prevent all
concurrent foreground puts. With WAL disabled, engine `sync` does not flush memtables
and its WAL step is a no-op. A durability claim needs a successfully completed write
followed by the appropriate persistence operation, not merely a call named `sync`.

SST construction syncs the output file; directory sync persists file-name changes;
the synced manifest records the live layout. These are separate steps. Output files
must be available before a manifest replacement becomes durable, and old inputs
remain until that edit succeeds. Checksums detect the covered byte corruptions;
they are not a proof against every corruption. For example, the manifest length is
not covered by its body checksum, and an incomplete tail is handled as a torn append.

Read [spawn_compaction_thread](mini-lsm/src/compact.rs#L419),
[trigger_flush](mini-lsm/src/compact.rs#L444), and
[MiniLsm::close](mini-lsm/src/lsm_storage.rs#L239). Worker waits hold no engine guard
between trigger calls. `trigger_flush` releases its threshold-check read guard before
calling flush. `close` takes worker-handle mutexes and joins workers before its final
WAL sync or no-WAL freeze/flush. Those named handle guards release on return; they
protect thread handles, not foreground access to the database.

Stop foreground operations before closing: joining workers does not reject writes
from another engine handle. `Drop` merely signals workers and does not join or promise
a final flush. The public `force_flush` helper is marked for tests and should likewise
be read without assuming it coordinates arbitrary concurrent foreground writes.

## 7. Transaction lifetime and commit

Read [new_txn](mini-lsm-mvcc/src/mvcc.rs#L81) and
[Transaction::drop](mini-lsm-mvcc/src/mvcc/txn.rs#L219). `new_txn` acquires `ts`, captures
the published clock, registers its reader count, and constructs the transaction before
releasing the guard on return. The registration persists without holding the mutex.
Final `Drop` briefly acquires `ts` to unregister. A scan's `Arc<Transaction>` extends
that lifetime; successful commit alone does not unregister the reader.

Read [Transaction::get](mini-lsm-mvcc/src/mvcc/txn.rs#L51),
[put](mini-lsm-mvcc/src/mvcc/txn.rs#L97), and
[TxnIterator::add_to_read_set](mini-lsm-mvcc/src/mvcc/txn.rs#L298).
With validation enabled, these acquire the transaction's `key_hashes` mutex only
around hash-set updates. They retain no such guard between operations. Point reads
record missing keys too; scans record visited live keys, including their initial
position, not gaps or a range predicate. Phantom inserts can go undetected.

Now read [Transaction::commit](mini-lsm-mvcc/src/mvcc/txn.rs#L127):

1. Mark the transaction one-shot, then acquire `commit_lock` through function return.
   The atomic flag does not lock the workspace. This implementation assumes one
   foreground thread operates on a given transaction and its iterators.
2. Validation acquires `key_hashes`, then `committed_txns` if there are writes.
   Compare our reads against writes committed after `read_ts`. Release the history
   mutex at the inner block's end and `key_hashes` at the outer branch's end.
   Validation failure releases guards, consumes the transaction, and publishes none
   of its workspace. A read-only commit skips conflict validation and publication.
3. Call `write_batch_inner`, which takes `write_lock` inside the still-held
   `commit_lock`. It releases `write_lock` on return after publication/maintenance.
4. For validation-enabled commits, acquire `committed_txns`, then `key_hashes` to
   register the write set. Briefly acquire/release `ts` through `watermark()` while
   those two guards remain held; remove history strictly below that watermark.
5. Release the two registration guards at their block's end and `commit_lock` at
   function return. Early returns release live guards too.

Notice that validation and registration take the history/hash-set locks in opposite
orders. The enclosing `commit_lock` serializes commits; there is **no universal lock
order** you should infer and reuse in new code. Trace each path's nested guards.

An I/O failure is different from a validation rejection. In particular, a freeze
failure after timestamp publication can exit commit **before history registration**.
The implementation does not provide rollback or a general guarantee of safe continued
serializable validation after that failure. The comments must not describe every
`Err` as an uncommitted transaction.

## 8. General component contracts and their boundaries

These are the reusable rules behind the examples—not rules specific to deletion.

| Component | Contract and caller responsibility |
| --- | --- |
| [MergeIterator](mini-lsm/src/iterators/merge_iterator.rs#L61) | Children are positioned, sorted, and individually unique by their exposed key. Emit keys in order; smaller input index wins equal-key conflicts. The caller supplies priority. |
| [TwoMergeIterator](mini-lsm/src/iterators/two_merge_iterator.rs#L27) | Same sorted/unique requirement; two child types expose the same key type. A wins ties. One B advance removes a tie only because B has no repeated equal keys. |
| [MVCC merge](mini-lsm-mvcc/src/iterators/merge_iterator.rs#L63) | Equality means identical `(user key, timestamp)`. `a@9` and `a@4` both survive; visibility is downstream. The transaction overlay instead compares user-key bytes, so local `a` overrides snapshot `a`. |
| [SstConcatIterator](mini-lsm/src/iterators/concat_iterator.rs#L35) | Input SST ranges must already be ordered and strictly disjoint under the key comparator. It visits one table cursor at a time; it does not resolve overlapping sources. |
| [LsmIterator](mini-lsm/src/lsm_iterator.rs#L44) / [MVCC LsmIterator](mini-lsm-mvcc/src/lsm_iterator.rs#L42) | Converts the raw merged stream into bounded live user entries; MVCC also chooses the newest visible version per user key. |
| [FusedIterator](mini-lsm/src/lsm_iterator.rs#L167) | Provides harmless advancement after exhaustion and permanent invalidation after an iteration error. Do not assume raw children, or every outer wrapper, independently provide this behavior. |
| [BlockBuilder](mini-lsm/src/block/builder.rs#L67) / [SsTableBuilder](mini-lsm/src/table/builder.rs#L53) | Consume sorted, representable entries supplied by callers; do not sort. Capacity rejection can be retried in a fresh block; an unrepresentable key/value cannot. |
| [Bloom filter](mini-lsm/src/table/bloom.rs#L126) | With a correctly built/decoded filter including every stored key, false rules out membership; true requires checking the table. This is not a general range-overlap test. |
| [Compaction controller](mini-lsm/src/compact.rs#L70) | Selects input ids and computes replacement layouts. The engine supplies locking, SST construction, publication, and persistence ordering. |
| [MVCC compaction](mini-lsm-mvcc/src/compact.rs#L130) | Ordinary GC keeps every version above the watermark and the newest at/below it, subject to safe bottom tombstone removal. Explicit prefix filters change the policy and exclude those prefixes from normal read guarantees. |

For example, A=`[b:9,d:4]`, B=`[b:2,c:3]` merges to `[b:9,c:3,d:4]` for reads or
compaction. Replacing A's b with a tombstone does not change duplicate resolution.
A read consumer suppresses the winning deletion; a non-bottom compaction consumer
preserves it in an output SST. The merge itself neither examines the value nor
chooses that consumer policy.

When reading any function, identify its **inputs/preconditions → state change →
output → lock/ownership lifetime → error behavior**. That is the common structure
behind the chapter examples, and the limit on how far to generalize each one.
