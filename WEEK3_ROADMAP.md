# Week 3 — versions, snapshots, and transactions

Use **`mini-lsm-mvcc/`** for this week. The `mini-lsm/` solution has Week 3 stubs
and links to starter files; it is not the completed MVCC implementation. These
links go directly to the completed, annotated code. It includes the final behavior
of later days, so intermediate refactor tasks must be read as parts of that result.

Keep four mechanisms separate:

| Mechanism | Question it answers |
| --- | --- |
| Internal-key ordering and merging | What is the next `(user key, timestamp)` entry? |
| Snapshot visibility | Which version of this user key was visible at `read_ts`? |
| Watermark GC | Which old versions can no active snapshot still need? |
| Commit validation | Did a newer committed write invalidate this transaction's tracked reads? |

A timestamp controls **visibility**. One WAL frame controls **recovery atomicity**.
With WAL enabled, a successfully completed write followed by successful WAL `sync`
provides the file **durability** boundary. Without WAL, persistence requires SST and
manifest work; engine `sync` does not flush memory. These are different guarantees.

For lock acquisition/release, ownership, preconditions, and error boundaries, keep
[Locks, ownership, and guarantees](LOCKING_AND_INVARIANTS.md) open alongside this guide.

## Day 1 — Timestamp Key Encoding + Refactor

Book: [Timestamp Key Encoding + Refactor](mini-lsm-book/src/week3-01-ts-key-refactor.md#timestamp-key-encoding--refactor). Goal: represent and preserve versions
through every layer before using timestamps for transactional visibility.

### Task 0: Use MVCC Key Encoding

Read [Key](mini-lsm-mvcc/src/key.rs#L21) → [Key::cmp](mini-lsm-mvcc/src/key.rs#L220), and inspect `TS_RANGE_BEGIN`/`TS_RANGE_END` nearby.
User keys sort ascending, timestamps descending: `a@9 < a@4 < b@12`. Thus versions
of one key are adjacent with the newest first. `KeySlice`, `KeyVec`, and `KeyBytes`
represent borrowed, mutable-owned, and shared-owned forms of the same identity.
**Check:** Where should a seek to `a@6` land among `[a@9,a@4,b@12]`?

### Task 1: Encode Timestamps in Blocks

Read [BlockBuilder::add](mini-lsm-mvcc/src/block/builder.rs#L65) → [BlockIterator::seek_to_offset](mini-lsm-mvcc/src/block/iterator.rs#L118) →
[BlockIterator::seek_to_key](mini-lsm-mvcc/src/block/iterator.rs#L144). Prefix compression affects only user-key bytes;
an explicit u64 timestamp follows the key suffix. Size estimation includes those
eight bytes, and decoding restores them into the internal key before comparison.

Two entries for `apple` may share every user-key byte while retaining different
timestamps. **Check:** Why would omitting the timestamp when the suffix is empty
collapse distinct versions?

### Task 2: Encoding Timestamps in SSTs

Read [BlockMeta::encode_block_meta](mini-lsm-mvcc/src/table.rs#L70) → [BlockMeta::decode_block_meta](mini-lsm-mvcc/src/table.rs#L116) →
[SsTableBuilder::add](mini-lsm-mvcc/src/table/builder.rs#L55). Block boundary keys include timestamps so searches use
the same order as the entries. Bloom fingerprints use only user-key bytes, allowing
one membership question to cover every version of a key.
**Check:** Why would hashing `a@9` be inappropriate for a request asking for `a` at
an arbitrary snapshot timestamp?

### Task 3: LSM Iterators

Read [HeapWrapper::cmp](mini-lsm-mvcc/src/iterators/merge_iterator.rs#L44) →
[LsmIterator::key](mini-lsm-mvcc/src/lsm_iterator.rs#L130). Merge equality now means equal **internal** keys, so a@9
and a@4 are different entries and both survive the raw merge. The public key method
strips the timestamp. Day 3's visibility logic decides which version to expose;
merging alone does not make that decision.
**Check:** Why must a raw merge retain a@4 even when a@9 exists?

### Task 4: Memtable

Read [MemTable](mini-lsm-mvcc/src/mem_table.rs#L35) → [MemTable::put_batch](mini-lsm-mvcc/src/mem_table.rs#L165) → [MemTable::scan](mini-lsm-mvcc/src/mem_table.rs#L196) →
[MemTable::flush](mini-lsm-mvcc/src/mem_table.rs#L212). In the completed solution the map is already keyed by
`KeyBytes`, so different timestamps coexist and flushing preserves them. The book's
Day 1 intermediate stage still uses default timestamps; later days supply real ones.
**Check:** Which field makes replacing a@9 different from inserting a@10?

### Task 5: Engine Read Path

Read [LsmStorageInner::get_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L547) → [LsmStorageInner::scan_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L878) and
[range_overlap](mini-lsm-mvcc/src/lsm_storage.rs#L165). Seeks use timestamped keys, while table-range pruning compares
user-key bytes. `a@MAX` is the beginning of a's full version range, not a real commit.
In the finished code these functions also apply read timestamps, covered below.
**Check:** Why must a table containing an older a version remain a candidate?

Read [test_sst_build_multi_version_simple](mini-lsm-mvcc/src/tests/week3_day1.rs#L26) and
[test_sst_build_multi_version_hard](mini-lsm-mvcc/src/tests/week3_day1.rs#L46).
Run: `cargo test -p mini-lsm-mvcc --lib week3_day1`.

## Day 2 — Snapshots: Memtables and Timestamps

Book: [Snapshot Read - Memtables and Timestamps](mini-lsm-book/src/week3-02-snapshot-read-part-1.md#snapshot-read---memtables-and-timestamps). Goal: assign real commit times and
make reads consistently select from versioned sources.

### Task 1: MemTable, Write-Ahead Log, and Read Path

Read [MemTable::put_batch](mini-lsm-mvcc/src/mem_table.rs#L165) → [Wal::put_batch](mini-lsm-mvcc/src/wal.rs#L130) →
[LsmStorageInner::get_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L547) → [map_key_bound_plus_ts](mini-lsm-mvcc/src/mem_table.rs#L67). Memory and WAL
store the timestamp with each entry. A point read scans the target user's version
range rather than performing an exact timestamp-free map lookup. Scan bounds must
account for descending timestamp order.

For versions `a@9=new,a@4=old`, a read at 6 must continue past a@9 to find a@4.
**Check:** Why is returning the first current-memtable hit no longer sufficient?

### Task 2: Write Path

Read [LsmMvccInner::latest_commit_ts](mini-lsm-mvcc/src/mvcc.rs#L62) → [LsmStorageInner::write_batch_inner](mini-lsm-mvcc/src/lsm_storage.rs#L629)
→ [LsmMvccInner::update_commit_ts](mini-lsm-mvcc/src/mvcc.rs#L67). Under `write_lock`, allocate one timestamp
for the whole batch, insert all its entries, then publish the timestamp. New
snapshots capture the published clock; older snapshots ignore entries above it.
`write_lock` stays acquired through the subsequent freeze attempt and releases on
function return. The state read guard ends before publication; each clock access
uses a separate short `ts` guard. See the [full write sequence](LOCKING_AND_INVARIANTS.md#4-writes-and-freeze-why-dropping-the-read-guard-matters).
A freeze error can return after publication, so `Err` does not always mean rollback.

At latest=5, a batch writes a@6 and b@6. Until publication, new readers still
capture 5 and see neither update. **Check:** Why must publication happen after
both insertions, and before fallible freeze maintenance?

### Task 3: MVCC Compaction

Read [LsmStorageInner::compact](mini-lsm-mvcc/src/compact.rs#L251) → [LsmStorageInner::compact_generate_sst_from_iter](mini-lsm-mvcc/src/compact.rs#L130).
The merge now preserves different timestamps of a user key. At this stage of the
course, the key requirement is not to collapse history as if it were Week 2 data.
The finished generator additionally applies Day 4 watermark GC and Day 7 filters;
read those branches in their later sections rather than treating them as merge rules.
**Check:** Why can compaction's output contain repeated user keys yet still contain
unique internal keys?

### Task 4: LSM Iterator

Read [LsmIterator::new](mini-lsm-mvcc/src/lsm_iterator.rs#L42) → [LsmIterator::move_to_key](mini-lsm-mvcc/src/lsm_iterator.rs#L80) → [LsmIterator::next](mini-lsm-mvcc/src/lsm_iterator.rs#L138).
The iterator walks raw internal keys but emits at most one visible user-key entry.
`prev_key` records which user key has already been resolved, so subsequent calls
skip its older history. Timestamp filtering and deletion filtering are separate steps.
**Check:** Why must selecting a visible tombstone also mark that user key resolved?

Read [test_timestamped_batches_and_latest_reads](mini-lsm-mvcc/src/tests/week3_day2.rs#L31).
Run: `cargo test -p mini-lsm-mvcc --lib week3_day2`.

## Day 3 — Snapshots: Engine Read Path and Transaction API

Book: [Snapshot Read - Engine Read Path and Transaction API](mini-lsm-book/src/week3-03-snapshot-read-part-2.md#snapshot-read---engine-read-path-and-transaction-api). Goal: hold a read timestamp across
operations, select the right version, and preserve the timestamp clock on restart.

### Task 1: LSM Iterator with Read Timestamp

Read [LsmMvccInner::new_txn](mini-lsm-mvcc/src/mvcc.rs#L81) → [LsmIterator::new](mini-lsm-mvcc/src/lsm_iterator.rs#L42) → [LsmIterator::move_to_key](mini-lsm-mvcc/src/lsm_iterator.rs#L80).
A transaction captures one `read_ts`. For each user key, skip versions above that
timestamp and inspect the first version at or below it. If live, return it; if a
tombstone, suppress the entire user key, including older live versions.

Trace `a@9=new,a@4=delete,a@1=old`: at read_ts=10 return new; at 6 return absent;
at 2 return old. **Check:** Why is a too-new tombstone irrelevant to an older snapshot?

### Task 2: Multi-Version Scan and Get

Read [Transaction::get](mini-lsm-mvcc/src/mvcc/txn.rs#L51) → [LsmStorageInner::get_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L547), then
[Transaction::scan](mini-lsm-mvcc/src/mvcc/txn.rs#L73) → [LsmStorageInner::scan_with_ts](mini-lsm-mvcc/src/lsm_storage.rs#L878) →
[map_key_bound_plus_ts](mini-lsm-mvcc/src/mem_table.rs#L67). The local workspace is a Day 5 addition; with it empty,
follow the shared snapshot path. An excluded lower user key must skip every version
of that key in SSTs, so the code uses `while`, not Week 1's single `if` advance.

For `[a@9,a@4,b@3]`, excluding a must start at b. Including upper a must permit
older a versions, so memory's upper internal bound uses a@0.
**Check:** Which comparisons concern full internal keys, and which concern only user bytes?

### Task 3: Store Largest Timestamp in SST

Read [SsTableBuilder::add](mini-lsm-mvcc/src/table/builder.rs#L55) → [SsTableBuilder::build](mini-lsm-mvcc/src/table/builder.rs#L101) →
[BlockMeta::encode_block_meta](mini-lsm-mvcc/src/table.rs#L70) → [SsTable::max_ts](mini-lsm-mvcc/src/table.rs#L398). Track the maximum across
**all** entries and persist it in metadata. The key with maximum timestamp need not
be the final key in lexical order; the first inserted key can hold that maximum.
**Check:** For `[a@20,z@3]`, what value must the SST report?

### Task 4: Recover Commit Timestamp

Read [LsmStorageInner::open](mini-lsm-mvcc/src/lsm_storage.rs#L374), particularly `last_commit_ts`, SST opening, WAL
replay, and construction of `LsmMvccInner`. Start from the maximum timestamp seen
in all surviving SSTs and recovered memory. The next batch uses a larger value.
File ids and manifest positions are unrelated to the commit clock.
**Check:** If SST max=20 and a live WAL contains ts=23, which timestamp comes next?

Read [test_task2_memtable_mvcc](mini-lsm-mvcc/src/tests/week3_day3.rs#L66),
[test_task2_all_range_bounds_across_l0_and_level_ssts](mini-lsm-mvcc/src/tests/week3_day3.rs#L370),
[test_task3_sst_ts](mini-lsm-mvcc/src/tests/week3_day3.rs#L396), and
[test_release_first_key_max_sst_restart_advances_timestamp](mini-lsm-mvcc/src/tests/release_regressions.rs#L198).
Run: `cargo test -p mini-lsm-mvcc --lib week3_day3`.

## Day 4 — Watermark and Garbage Collection

Book: [Watermark and Garbage Collection](mini-lsm-book/src/week3-04-watermark.md#watermark-and-garbage-collection). Goal: reclaim history without invalidating
ordinary reads held by live snapshots.

### Task 1: Implement Watermark

Read [Watermark::add_reader](mini-lsm-mvcc/src/mvcc/watermark.rs#L34) → [Watermark::remove_reader](mini-lsm-mvcc/src/mvcc/watermark.rs#L40) →
[Watermark::watermark](mini-lsm-mvcc/src/mvcc/watermark.rs#L54) → [LsmMvccInner::watermark](mini-lsm-mvcc/src/mvcc.rs#L75). Store a reader count per
timestamp, and return the smallest active timestamp. With no active readers, the
MVCC layer uses the latest committed timestamp.

Readers `[5,5,8]` have watermark 5. Dropping one reader at 5 leaves it unchanged;
dropping the other moves it to 8. **Check:** Why would a set of timestamps be insufficient?

### Task 2: Maintain Watermark in Transactions

Read [LsmMvccInner::new_txn](mini-lsm-mvcc/src/mvcc.rs#L81) → [Transaction::drop](mini-lsm-mvcc/src/mvcc/txn.rs#L219) → [TxnIterator](mini-lsm-mvcc/src/mvcc/txn.rs#L273).
Capture the timestamp and register the reader while holding the same mutex. The
transaction's final `Drop` unregisters it under a newly acquired short `ts` guard.
The `new_txn` guard releases when construction returns; the registry entry, not a
held mutex, protects the snapshot. Commit does not unregister it. A scan owns an `Arc<Transaction>`, so
its snapshot remains protected even after the caller drops their transaction handle.
**Check:** What race becomes possible if capturing and registering happen under separate locks?

### Task 3: Garbage Collection in Compaction

Read [LsmStorageInner::compact_generate_sst_from_iter](mini-lsm-mvcc/src/compact.rs#L130). Keep all versions newer
than the watermark and the newest version at or below it as the baseline. Versions
older than that baseline can be removed. The watermark mutex releases inside
`watermark()`; the copied value is retained throughout the rewrite. The filter-list
mutex likewise releases immediately after cloning the policies. The bottom-level tombstone case can remove
an entire obsolete deleted history; above bottom, deletion markers may still be needed.

At watermark=6, `[a@9,a@7,a@4,a@1]` retains 9,7,4. The a@4 version is still needed
by a reader at 6 even though its timestamp is lower. Also inspect the output split:
it waits for a different **user key**, keeping one key's versions in one output SST.
**Check:** Why is "delete every version below the watermark" incorrect?

Read [test_task1_watermark](mini-lsm-mvcc/src/tests/week3_day4.rs#L95),
[test_task2_snapshot_watermark](mini-lsm-mvcc/src/tests/week3_day4.rs#L133),
[test_task3_mvcc_compaction](mini-lsm-mvcc/src/tests/week3_day4.rs#L154), and
[test_task3_compaction_keeps_versions_together](mini-lsm-mvcc/src/tests/week3_day4.rs#L44).
Run: `cargo test -p mini-lsm-mvcc --lib week3_day4`.

## Day 5 — Transaction Workspace and Atomic Commit

Book: [Transaction Workspace and Atomic Commit](mini-lsm-book/src/week3-05-txn-occ.md#transaction-workspace-and-atomic-commit). Goal: stage private changes, read them locally,
then publish and recover one complete transaction.

### Task 1: Local Workspace + Put and Delete

Read [Transaction](mini-lsm-mvcc/src/mvcc/txn.rs#L41) → [Transaction::put](mini-lsm-mvcc/src/mvcc/txn.rs#L97) → [Transaction::delete](mini-lsm-mvcc/src/mvcc/txn.rs#L112).
The workspace is a private user-key map without timestamps. Repeated puts replace
its local entry; delete stores a tombstone rather than removing it. Nothing enters
the shared memtable or WAL yet. Timestamps are assigned when the batch commits.
**Check:** If shared a=1 and the transaction deletes a, why must its local map retain a?

### Task 2: Get and Scan

Read [Transaction::get](mini-lsm-mvcc/src/mvcc/txn.rs#L51) → [Transaction::scan](mini-lsm-mvcc/src/mvcc/txn.rs#L73) → [TxnIterator::create](mini-lsm-mvcc/src/mvcc/txn.rs#L279)
→ [TxnIterator::skip_deletes](mini-lsm-mvcc/src/mvcc/txn.rs#L291) → [TxnIterator::next](mini-lsm-mvcc/src/mvcc/txn.rs#L327). Local entries override
shared snapshot entries on equal user keys. The two-way merge resolves that conflict;
only afterward does the transaction iterator hide local deletion markers.

For shared `[a:1,b:2]` and local `[a:9,b:delete,c:3]`, the transaction scans
`[a:9,c:3]`, while another old snapshot still sees `[a:1,b:2]`.
**Check:** Which input to TwoMergeIterator must contain the local workspace?

### Task 3: Commit

Read [Transaction::commit](mini-lsm-mvcc/src/mvcc/txn.rs#L127) → [LsmStorageInner::write_batch_inner](mini-lsm-mvcc/src/lsm_storage.rs#L629) →
[MemTable::put_batch](mini-lsm-mvcc/src/mem_table.rs#L165). The transaction becomes one-shot, collects its workspace
into a batch, writes one timestamp into one memtable, then publishes that timestamp.
An empty batch returns without allocating a new timestamp. Day 6 adds validation
around this path; it is already present in the completed function.
Use one foreground thread per transaction and its iterators: the atomic committed
flag is not a lock around concurrent workspace edits. Separate transactions can
run concurrently. See [transaction lock lifetimes and failure cases](LOCKING_AND_INVARIANTS.md#7-transaction-lifetime-and-commit).
**Check:** Why should a batch exceeding the target size finish before freezing?

### Task 4: Atomic WAL

Read [Wal::put_batch](mini-lsm-mvcc/src/wal.rs#L130) → [Wal::recover](mini-lsm-mvcc/src/wal.rs#L46) → [MemTable::put_batch](mini-lsm-mvcc/src/mem_table.rs#L165).
One frame contains `body_size:u32 | (key_len,key,ts,value_len,value)* | CRC:u32`.
Recovery decodes into temporary pairs, validates the entire frame, then applies
all pairs. An incomplete final frame is truncated; a complete corrupt frame errors
without exposing its valid-looking prefix. Earlier validated frames remain replayed.

Trace complete batch A followed by half of B: recover all of A and none of B.
WAL append precedes map mutation, and publication follows the whole insertion.
**Check:** Why does atomic replay still not imply durability before `sync()`?

Read [test_txn_integration](mini-lsm-mvcc/src/tests/week3_day5.rs#L30),
[test_task4_batch_uses_one_memtable_and_timestamp](mini-lsm-mvcc/src/tests/week3_day5.rs#L95),
[test_task4_wal_ignores_and_truncates_incomplete_final_batch](mini-lsm-mvcc/src/tests/week3_day5.rs#L152), and
[test_task4_wal_recovery_is_atomic_across_frames](mini-lsm-mvcc/src/tests/week3_day5.rs#L201).
Run: `cargo test -p mini-lsm-mvcc --lib week3_day5`.

## Day 6 — Serializable Validation

Book: [Serializable Validation (with a Scan Limitation)](mini-lsm-book/src/week3-06-serializable.md#serializable-validation-with-a-scan-limitation). Goal: validate tracked dependencies against
newer commits. This is the course's conservative point-read rule, not a complete
SSI implementation; scan gaps are not tracked, so phantoms remain possible.

### Task 1: Track Read Set in Get and Write Set

Read [LsmMvccInner::new_txn](mini-lsm-mvcc/src/mvcc.rs#L81) → [Transaction::get](mini-lsm-mvcc/src/mvcc/txn.rs#L51) →
[Transaction::put](mini-lsm-mvcc/src/mvcc/txn.rs#L97) → [Transaction::delete](mini-lsm-mvcc/src/mvcc/txn.rs#L112). Enable hash sets when
`serializable` is true. Every point-read key enters the read set even when absent;
puts and deletes enter the write set. Hash collisions can cause false conflicts.
**Check:** If a transaction reads missing a and another inserts a, why is the first
transaction's later dependent write subject to validation?

### Task 2: Track Read Set in Scan

Read [TxnIterator::create](mini-lsm-mvcc/src/mvcc/txn.rs#L279) → [TxnIterator::add_to_read_set](mini-lsm-mvcc/src/mvcc/txn.rs#L298) →
[TxnIterator::next](mini-lsm-mvcc/src/mvcc/txn.rs#L327). Track the initial returned key and later returned keys.
This records visited entries, not the range predicate or gaps. An empty scan records
no key hashes; a concurrent insertion into that range may therefore go undetected.
**Check:** Why does hashing returned keys fail to represent "no key exists in [a,z]"?

### Task 3: Engine Interface and Serializable Validation

Read [Transaction::commit](mini-lsm-mvcc/src/mvcc/txn.rs#L127) → [LsmStorageInner::write_batch](mini-lsm-mvcc/src/lsm_storage.rs#L676) →
[LsmStorageInner::write_batch_inner](mini-lsm-mvcc/src/lsm_storage.rs#L629). Hold `commit_lock` across validation,
publication, and insertion of the committed write-set record. Intersect this
transaction's reads with writes committed after its `read_ts`. Ordinary engine
writes must participate in that history when validation is enabled.
`commit_lock` releases on return. Hash-set/history guards use smaller nested scopes:
validation releases them before publication; registration reacquires them afterward.
A post-publication freeze error can return before history registration, so the code
has no general rollback or continued-validation guarantee after that I/O failure.

T1 reads b/writes a; T2 reads a/writes b; both start at 10. If T1 commits at 11,
T2's read(a) intersects T1's write(a), so T2 aborts without publication. Two blind
writes can instead serialize by commit order. Read-only commits skip validation.
**Check:** Why would releasing the lock between validation and publication reopen a race?

### Task 4: Garbage Collection

Read the `committed_txns` insertion and watermark cleanup at the end of
[Transaction::commit](mini-lsm-mvcc/src/mvcc/txn.rs#L127), then [LsmMvccInner::watermark](mini-lsm-mvcc/src/mvcc.rs#L75). Remove commit-history
records strictly below the watermark. This reclaims conflict metadata, not SST
versions; version GC has the separate per-key baseline rule from Day 4.
**Check:** Why can an old live transaction keep both history metadata and old values alive?

Read [test_serializable_1](mini-lsm-mvcc/src/tests/week3_day6.rs#L27),
[test_serializable_2](mini-lsm-mvcc/src/tests/week3_day6.rs#L46),
[test_serializable_3_ts_range](mini-lsm-mvcc/src/tests/week3_day6.rs#L61), and
[test_serializable_4_scan](mini-lsm-mvcc/src/tests/week3_day6.rs#L80). The scan test checks conflicts
on returned keys; it does not establish protection against arbitrary phantoms.
Run: `cargo test -p mini-lsm-mvcc --lib week3_day6`.

## Day 7 — Compaction Filters

Book: [Snack Time: Compaction Filters](mini-lsm-book/src/week3-07-compaction-filter.md#snack-time-compaction-filters). Goal: apply an explicit policy for
removing a key prefix while compaction rewrites files.

### Task 1: Compaction Filter

Read [CompactionFilter](mini-lsm-mvcc/src/lsm_storage.rs#L197) → [MiniLsm::add_compaction_filter](mini-lsm-mvcc/src/lsm_storage.rs#L306) →
[LsmStorageInner::add_compaction_filter](mini-lsm-mvcc/src/lsm_storage.rs#L525) →
[LsmStorageInner::compact_generate_sst_from_iter](mini-lsm-mvcc/src/compact.rs#L130). Registering the prefix
stores policy; it does not immediately rewrite files or free space. The compactor
keeps versions above the watermark and filters the baseline at or below it along
with older history of the matching key.

At watermark=5, matching `[k@8,k@5,k@2]` retains k@8. Once the watermark advances
to 8 and compaction visits it again, that history can disappear too.
The book explicitly assumes callers do not read inside filtered prefixes: matching
versions can disappear from different levels at different times. This is an explicit
deletion policy, not ordinary snapshot-preserving GC or an immediate range delete.
**Check:** Why is installing a filter alone insufficient to reclaim disk space?

Read [test_task1_compaction_filter](mini-lsm-mvcc/src/tests/week3_day7.rs#L26). Notice that it inspects
raw stored versions before and after dropping a snapshot and compacting again.
Run: `cargo test -p mini-lsm-mvcc --lib week3_day7`.

## End-of-week trace

Begin with a@1=old. Hold a transaction at 1, commit a@2=new, flush, and compact.
Explain why the old reader still sees old. Drop every handle and iterator retaining
that reader, compact again, and identify which version may now disappear. Separately,
stage a two-key transaction, trace its single WAL frame, and place a crash in the
middle of its final append to reason about atomic recovery.

The book's optional bonuses extend the model (for example predicate tracking for
scan phantoms). The reference code above does not implement those extensions merely
because it passes the core chapter tests.
