# Week 2 — compaction and persistence

Use the annotated `mini-lsm/` solution. Read Week 1 first, especially merge
precedence, SST construction, and the freeze/flush distinction.

The big picture has three separate parts: a **controller chooses input files**;
the **compactor merges and rewrites them**; the **installation and recovery path
records which files form the live database**. A WAL records unflushed user writes.
A manifest records layout changes. Neither substitutes for the other.

## Day 1 — Compaction Implementation

Book: [Compaction Implementation](mini-lsm-book/src/week2-01-compaction.md#compaction-implementation). Goal: rewrite multiple sources into fewer
sorted files without changing visible results.

### Task 1: Compaction Implementation

Read [CompactionTask::compact_to_bottom_level](mini-lsm/src/compact.rs#L52) →
[LsmStorageInner::compact](mini-lsm/src/compact.rs#L190) → [LsmStorageInner::compact_generate_sst_from_iter](mini-lsm/src/compact.rs#L130)
→ [LsmStorageInner::force_full_compaction](mini-lsm/src/compact.rs#L285). The task specifies selected files;
`compact` assembles their merge tree; the generator writes output SSTs; the final
function installs the result. Compare the merge ordering with the read path:
compaction must choose the same winning entry for each duplicate key.

At non-bottom levels, retain a winning tombstone because it may hide an older
value below the selected inputs. At the bottom, it can disappear. If every input
entry disappears, output can legitimately be an empty list—do not build an empty SST.
**Check:** Why is filtering tombstones before the merge wrong here too?

### Task 2: Concat Iterator

Read [SstConcatIterator::check_sst_valid](mini-lsm/src/iterators/concat_iterator.rs#L35) →
[SstConcatIterator::create_and_seek_to_key](mini-lsm/src/iterators/concat_iterator.rs#L68) →
[SstConcatIterator::move_until_valid](mini-lsm/src/iterators/concat_iterator.rs#L94) → [SstConcatIterator::next](mini-lsm/src/iterators/concat_iterator.rs#L133).
This cursor visits one SST at a time because its input ranges are sorted and
strictly disjoint. In `[a..f],[m..r]`, seeking `h` can exhaust the first file and
continue at `m`. Overlapping L0 files cannot use this shortcut.
**Check:** What breaks if an earlier file ends at `z` while a later one starts at `m`?

### Task 3: Integrate with the Read Path

Read [LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L808) and [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L517), focusing on their
`level_iters` loops, then [LsmIteratorInner](mini-lsm/src/lsm_iterator.rs#L30). Concatenate files **within** a
level/tier; merge streams **across** levels/tiers; put the memory/L0 group ahead
of these older sources. The extra nesting changes physical source coverage, not
the user-facing sorted, unique, live-key contract.
**Check:** Why is concatenation insufficient across two levels even though each
individual level has disjoint files?

Read [test_task1_full_compaction](mini-lsm/src/tests/week2_day1.rs#L48),
[test_task1_full_compaction_all_tombstones](mini-lsm/src/tests/week2_day1.rs#L31),
[test_task2_concat_iterator](mini-lsm/src/tests/week2_day1.rs#L188), and
[test_task3_integration](mini-lsm/src/tests/week2_day1.rs#L224).
Run: `cargo test -p mini-lsm --lib week2_day1`.

## Day 2 — Simple Compaction Strategy

Book: [Simple Compaction Strategy](mini-lsm-book/src/week2-02-simple.md#simple-compaction-strategy). Goal: choose whole adjacent levels using file
counts, then schedule those tasks in the background.

### Task 1: Simple Leveled Compaction

Read [SimpleLeveledCompactionController::generate_compaction_task](mini-lsm/src/compact/simple_leveled.rs#L50) →
[SimpleLeveledCompactionController::apply_compaction_result](mini-lsm/src/compact/simple_leveled.rs#L112). L0's file-count
threshold has priority. Otherwise compare `lower file count / upper file count`
with the configured percentage and compact the selected whole levels.

For upper=4 files and lower=2, the ratio is 50%; a threshold of 200% triggers.
When installing an L0 task planned for `[8,7]`, a concurrently flushed 9 must leave
L0 as `[9]`, not `[]`. Removal uses the task's exact ids against the current state.
**Check:** Why must the preserved file 9 stay first?

### Task 2: Compaction Thread

Read [LsmStorageInner::spawn_compaction_thread](mini-lsm/src/compact.rs#L412) →
[LsmStorageInner::trigger_compaction](mini-lsm/src/compact.rs#L348) → [CompactionController::generate_compaction_task](mini-lsm/src/compact.rs#L70)
→ [CompactionController::apply_compaction_result](mini-lsm/src/compact.rs#L85). Planning uses a cloned
layout; expensive file construction runs outside the state RwLock; installation
acquires the structural lock and rereads the current state.

Follow output creation → publish replacement layout → sync directory/manifest →
delete obsolete inputs. The manifest must refer to available output files before
inputs disappear. **Check:** Why would installing the original planning snapshot
lose a flush that completed during the file build?

### Task 3: Integrate with the Read Path

Revisit [LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L808) and [SstConcatIterator::create_and_seek_to_first](mini-lsm/src/iterators/concat_iterator.rs#L48).
Now several levels may contain candidates. Each contributes a sorted run; earlier
levels win equal keys. The concat invariant depends on compaction producing sorted,
disjoint output files and result application preserving their order.
**Check:** Trace `a:new` in L1 and `a:old` in L2 through the level merge.

Read [test_integration](mini-lsm/src/tests/week2_day2.rs#L28) and
[test_l0_compaction_preserves_newer_ssts_in_order](mini-lsm/src/tests/week2_day2.rs#L47).
Run: `cargo test -p mini-lsm --lib week2_day2`.

## Day 3 — Tiered Compaction Strategy

Book: [Tiered Compaction Strategy](mini-lsm-book/src/week2-03-tiered.md#tiered-compaction-strategy). Goal: organize newest-first sorted runs and
merge selected adjacent runs when their count or size balance becomes undesirable.

### Task 1: Universal Compaction

Read [TieredCompactionController::generate_compaction_task](mini-lsm/src/compact/tiered.rs#L45) from top to bottom.
The order of its triggers is part of the policy; they are not independent jobs.
This implementation estimates run sizes by file count.

#### Task 1.0: Precondition

Start at [TieredCompactionController::generate_compaction_task](mini-lsm/src/compact/tiered.rs#L45) and compare
[CompactionController::flush_to_l0](mini-lsm/src/compact.rs#L108) with [LsmStorageInner::force_flush_next_imm_memtable](mini-lsm/src/lsm_storage.rs#L742).
Tiered flushing creates a new tier at the front. L0 must remain empty. The scheduler
returns early until the configured number of tiers is present.
**Check:** Why would also adding the file to L0 read the same source twice?

#### Task 1.1: Triggered by Space Amplification Ratio

Read [space-amplification calculation](mini-lsm/src/compact/tiered.rs#L59).
Sum newer tiers and divide by the oldest tier, then compare with the configured
percentage. With counts `[2,3,5]`, the estimate is `(2+3)/5*100 = 100%`.
This trigger selects all tiers and marks the bottom included.
**Check:** Why can this task potentially remove tombstones?

#### Task 1.2: Triggered by Size Ratio

Read [size-ratio prefix selection](mini-lsm/src/compact/tiered.rs#L78).
Accumulate a prefix of newer runs and compare the next older run with that sum.
When the next run is sufficiently larger and the prefix reaches `min_merge_width`,
merge the prefix. The selected prefix excludes that next run and the bottom tier.
**Check:** Which side of the ratio is the older run, and which runs actually enter the task?

#### Task 1.3: Reduce Sorted Runs

Read [run-count fallback](mini-lsm/src/compact/tiered.rs#L106) →
[TieredCompactionController::apply_compaction_result](mini-lsm/src/compact/tiered.rs#L123). If earlier tests did not
select work, take a prefix capped by `max_merge_width`. Preserve other tiers and
insert the replacement at the selected runs' position. Empty output is allowed.
**Check:** If only three of five tiers are selected, may this task remove all tombstones?

### Task 2: Integrate with the Read Path

Read the tiered arm of [LsmStorageInner::compact](mini-lsm/src/compact.rs#L190) and the level/tier loop in
[LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L808). A tier can contain several disjoint SSTs, so concatenate
within each tier. Different tiers overlap, so merge them newest first.
**Check:** Why is a tier id not a user-key ordering for its files?

Read [test_reduce_sorted_runs_respects_max_merge_width](mini-lsm/src/tests/week2_day3.rs#L49),
[test_tiered_compaction_accepts_empty_output](mini-lsm/src/tests/week2_day3.rs#L73), and
[test_integration](mini-lsm/src/tests/week2_day3.rs#L28).
Run: `cargo test -p mini-lsm --lib week2_day3`.

## Day 4 — Leveled Compaction Strategy

Book: [Leveled Compaction Strategy](mini-lsm-book/src/week2-04-leveled.md#leveled-compaction-strategy). Goal: compact a selected SST and its overlapping
neighbors, using byte-based level targets rather than always rewriting whole levels.

### Task 1: Leveled Compaction

Start at [LeveledCompactionController::generate_compaction_task](mini-lsm/src/compact/leveled.rs#L81). Follow these
four calculations before looking at result installation.

#### Task 1.1: Compute Target Sizes

Read [actual and target size calculation](mini-lsm/src/compact/leveled.rs#L85).
Sum actual SST bytes per level. Start the bottom target at the larger of its actual
size and the base size, then divide targets backward by the multiplier. Targets
for sufficiently shallow levels stay zero rather than demanding data at every level.
**Check:** Why does changing the byte multiplier affect how many levels are active?

#### Task 1.2: Decide Base Level

Read [base-level calculation](mini-lsm/src/compact/leveled.rs#L103)
then [L0 destination selection](mini-lsm/src/compact/leveled.rs#L117).
The shallowest positive target is the base level. For four levels, bottom=1000 MB,
base size=100 MB, multiplier=10, targets become `[0,0,100,1000]`: L0 enters L3.
**Check:** Why would always flushing L0 into L1 defeat these dynamic targets?

#### Task 1.3: Decide Level Priorities

Read [priority scoring](mini-lsm/src/compact/leveled.rs#L133). Score each level
as `actual/target`, keep scores above 1, and select the highest. L0's count trigger
has already been considered first. A level with score 2 is twice its target size.
**Check:** Why is comparing absolute byte sizes a different scheduling policy?

#### Task 1.4: Select SST to Compact

Read [oldest-SST selection](mini-lsm/src/compact/leveled.rs#L159) →
[LeveledCompactionController::find_overlapping_ssts](mini-lsm/src/compact/leveled.rs#L48). Select the oldest SST id
in the chosen level, then include lower-level tables touching its key interval.
For input `[b,f]`, lower `[e,h]` participates and `[i,m]` does not.
**Check:** Why must lower files touching the boundary participate too?

### Task 2: Integrate Leveled Compaction

Read [LeveledCompactionController::apply_compaction_result](mini-lsm/src/compact/leveled.rs#L179) →
[LsmStorageInner::trigger_compaction](mini-lsm/src/compact.rs#L348) → [LsmStorageInner::open](mini-lsm/src/lsm_storage.rs#L362). Remove only
selected ids, add outputs, and sort the lower level by first key. During recovery,
tables have not been opened yet, so sorting waits until their metadata is available.
**Check:** Why does the installer add output objects to `sstables` before applying
this controller's result?

Read [test_integration](mini-lsm/src/tests/week2_day4.rs#L28),
[test_l0_compaction_preserves_newer_ssts_in_order](mini-lsm/src/tests/week2_day4.rs#L48), and
[test_multiple_compacted_ssts_leveled](mini-lsm/src/tests/week2_day5.rs#L65).
Run: `cargo test -p mini-lsm --lib week2_day4`.

## Day 5 — Manifest

Book: [Manifest](mini-lsm-book/src/week2-05-manifest.md#manifest). Goal: recover the live source layout after
restart. Directory contents alone cannot identify which SSTs are still live.

### Task 1: Manifest Encoding

Read [ManifestRecord](mini-lsm/src/manifest.rs#L32) → [Manifest::add_record_when_init](mini-lsm/src/manifest.rs#L139) → [Manifest::recover](mini-lsm/src/manifest.rs#L54).
Records describe new memtables, completed flushes, and compaction replacements.
Each frame is `body length:u64 | JSON body | CRC:u32`. Recovery replays complete
validated frames, truncates an incomplete final frame, and rejects a complete
frame with a bad checksum.
**Check:** Why are a torn append and a corrupt complete record treated differently?

### Task 2: Write Manifests

Read [Manifest::add_record](mini-lsm/src/manifest.rs#L131) and its callers in
[LsmStorageInner::force_freeze_memtable](mini-lsm/src/lsm_storage.rs#L714),
[LsmStorageInner::force_flush_next_imm_memtable](mini-lsm/src/lsm_storage.rs#L742), and
[LsmStorageInner::trigger_compaction](mini-lsm/src/compact.rs#L348). Trace which files are created/synced
before recording an edit and which old files are deleted afterward. The passed
`MutexGuard` indicates that structural updates are serialized across this sequence.
**Check:** After a crash before the manifest edit, which layout will recovery choose?

### Task 3: Flush on Close

Read [MiniLsm::close](mini-lsm/src/lsm_storage.rs#L234). Stop and join workers before final persistence. Without
WAL, freeze current memory and flush all immutable maps; with WAL, the later path
syncs the log instead. Explicit `close()` does more than the handle's `Drop`.
**Check:** Why does merely notifying background threads not establish persistence?

### Task 4: Recover from the State

Read [LsmStorageInner::open](mini-lsm/src/lsm_storage.rs#L362), first the manifest replay loop, then the SST-open
loop, then the WAL/memtable reconstruction. `NewMemtable(7)` adds a pending memory
source; `Flush(7)` removes that pending id and adds an SST; a compaction record
replaces selected ids. Open surviving SSTs only after replay determines the layout.
**Check:** Why should an obsolete SST file still present on disk not be reopened?

Read [test_integration_leveled](mini-lsm/src/tests/week2_day5.rs#L30),
[test_multiple_compacted_ssts_leveled](mini-lsm/src/tests/week2_day5.rs#L65), and
[test_release_manifest_torn_tail_preserves_durable_prefix](mini-lsm/src/tests/release_regressions.rs#L70).
Run: `cargo test -p mini-lsm --lib week2_day5`.

## Day 6 — Write-Ahead Log

Book: [Write-Ahead Log (WAL)](mini-lsm-book/src/week2-06-wal.md#write-ahead-log-wal). Goal: recover writes that have not reached an SST.
The manifest identifies which WALs matter; each WAL supplies their data records.

### Task 1: WAL Encoding

Read [Wal::put](mini-lsm/src/wal.rs#L115) → [Wal::recover](mini-lsm/src/wal.rs#L46) → [Wal::sync](mini-lsm/src/wal.rs#L145). The Week 2 record is
`key_len:u16 | key | value_len:u16 | value | CRC:u32`. Replay in append order so
later records replace earlier ones. A zero-length value replays as a tombstone.
`put` writes through `BufWriter`; `sync` flushes that buffer and syncs the file.
**Check:** Why is successful append not the same guarantee as successful sync?

### Task 2: Integrate WALs

Read [MemTable::create_with_wal](mini-lsm/src/mem_table.rs#L65) → [MemTable::put](mini-lsm/src/mem_table.rs#L115) →
[LsmStorageInner::force_freeze_memtable](mini-lsm/src/lsm_storage.rs#L714) → [LsmStorageInner::force_flush_next_imm_memtable](mini-lsm/src/lsm_storage.rs#L742).
Each memtable owns a WAL with the same id. Rotation creates a new WAL; successful
flush makes the old WAL unnecessary only after the SST and manifest record are safe.
The Week 2 `put` mutates memory before its WAL call; Week 3's batch path changes
that ordering and adds atomic publication. Do not infer Week 3 guarantees here.
**Check:** Why must the old WAL survive until its flush is recorded?

### Task 3: Recover from the WALs

Read [MemTable::recover_from_wal](mini-lsm/src/mem_table.rs#L75) → [Wal::recover](mini-lsm/src/wal.rs#L46) →
[LsmStorageInner::open](mini-lsm/src/lsm_storage.rs#L362). Manifest replay identifies unflushed ids; replay their
WALs into immutable maps and create a new writable map. Incomplete trailing records
are truncated so a future append does not follow uninterpretable tail bytes.
**Check:** With a valid record A followed by half of B, which record can be recovered?

Read [test_integration_leveled](mini-lsm/src/tests/week2_day6.rs#L27),
[test_release_wal_torn_tail_preserves_durable_prefix](mini-lsm/src/tests/release_regressions.rs#L167), and
[test_release_wal_corruption_and_oversized_fields_fail_closed](mini-lsm/src/tests/release_regressions.rs#L202).
Run: `cargo test -p mini-lsm --lib week2_day6`.

## Day 7 — Batch Write and Checksums

Book: [Batch Write and Checksums](mini-lsm-book/src/week2-07-snacks.md#batch-write-and-checksums). Goal: define the batch API and detect invalid
persistent bytes. Checksums and structural validation solve different problems.

### Task 1: Write Batch Interface

Read [WriteBatchRecord](mini-lsm/src/lsm_storage.rs#L64) → [validate_write_batch](mini-lsm/src/lsm_storage.rs#L69) → [LsmStorageInner::write_batch](mini-lsm/src/lsm_storage.rs#L611).
The API accepts puts and deletes in sequence. Validate field widths before the
loop; each entry then uses the current memtable and may trigger a freeze. This
Week 2 batch is **not atomic** for concurrent readers or crash recovery.
**Check:** Where could a two-entry batch span two memtables?

### Task 2: Block Checksum

Read [SsTableBuilder::finish_block](mini-lsm/src/table/builder.rs#L83) → [SsTable::read_block](mini-lsm/src/table.rs#L310) →
[Block::decode_checked](mini-lsm/src/block.rs#L51). A CRC follows each encoded block. Validate it before
decoding, then still validate offsets, lengths, and entry boundaries. A checksum
only checks agreement with the stored bytes; it cannot prove their format is valid.
**Check:** Which four bytes are excluded from the block data passed to the decoder?

### Task 3: SST Meta Checksum

Read [BlockMeta::encode_block_meta](mini-lsm/src/table.rs#L63) → [BlockMeta::decode_block_meta](mini-lsm/src/table.rs#L98) and
[Bloom::encode](mini-lsm/src/table/bloom.rs#L84) → [Bloom::decode](mini-lsm/src/table/bloom.rs#L63). Metadata and Bloom filters have their own
CRC checks. In this implementation, the metadata CRC excludes its leading count,
so count/length validation remains necessary. SST trailer offsets also need bounds checks.
**Check:** Can a well-sized file still contain an out-of-range block offset?

### Task 4: WAL Checksum

Read [Wal::put](mini-lsm/src/wal.rs#L115) → [Wal::recover](mini-lsm/src/wal.rs#L46). The checksum covers encoded length fields
as well as key/value payloads. Recovery inserts a record only after validating its
whole frame. This is record-level validation; Week 3 extends it to whole batches.
**Check:** Why must a checksum be checked before updating the recovered skipmap?

### Task 5: Manifest Checksum

Read [Manifest::add_record_when_init](mini-lsm/src/manifest.rs#L139) → [Manifest::recover](mini-lsm/src/manifest.rs#L54). The checksum
covers the JSON body; the u64 framing length is outside that checksum and is
separately checked before slicing. A valid prefix can survive a torn final append,
while an invalid complete body must fail recovery.
**Check:** Why should recovery avoid silently treating every checksum error as EOF?

There is no separate `week2_day7.rs` in this solution. Read
[test_release_sst_corruption_and_truncation_never_panic](mini-lsm/src/tests/release_regressions.rs#L135),
[test_release_manifest_corruption_never_panics](mini-lsm/src/tests/release_regressions.rs#L108), and
[test_release_public_write_size_boundaries](mini-lsm/src/tests/release_regressions.rs#L232).
Run: `cargo test -p mini-lsm --lib release_regressions`.

## End-of-week trace

Choose a write that is still in a WAL, a completed flush, and a completed compaction.
For each, identify the user record, live file ids, manifest edit, and sync boundary.
Then place a crash before and after that edit and predict the recovered layout.
Keep logical merge correctness separate from persistence ordering throughout.

Optional book bonuses explore different scheduling, compaction concurrency, or
persistence designs. Their completed implementations are not implied by these links.
