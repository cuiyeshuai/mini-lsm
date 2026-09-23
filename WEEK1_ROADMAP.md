# Week 1 — build one logical storage engine

Read this alongside the numbered tasks in the book. Links open the actual annotated
functions in `mini-lsm/`, the completed Week 1–2 solution. Later additions such as
WAL, checksums, and compaction appear in these files already; the notes tell you
which part belongs to the current task. Read the functions in the order shown.

The overall flow is **accept writes in sorted memory → freeze → encode blocks and
SSTs → merge memory and disk for reads**. A merge first orders keys and resolves
conflicting copies; a visibility wrapper subsequently removes winning tombstones.
Deletion is one application of the merge rule, not the merge's primary purpose.

For lock acquisition/release, ownership, preconditions, and error boundaries, keep
[Locks, ownership, and guarantees](LOCKING_AND_INVARIANTS.md) open alongside this guide.

## Day 1 — Memtables

Book: [Memtables](mini-lsm-book/src/week1-01-memtable.md#memtables). Goal: keep a latest value in memory and rotate
full maps without losing the ability to read older data.

### Task 1: SkipList Memtable

Read [MemTable::create](mini-lsm/src/mem_table.rs#L53) → [MemTable::put](mini-lsm/src/mem_table.rs#L115) → [MemTable::get](mini-lsm/src/mem_table.rs#L106).
The skipmap gives sorted keys and interior mutation, while `Bytes` provides owned,
cheaply cloned buffers. At this stage there is one stored entry per user key.
The WAL branch in `put` is a Week 2 addition; first follow the map insertion.

Trace `put(a,1)`, `put(a,2)`, `get(a)`: the result is 2. The size counter grows for
both puts, so it estimates submitted data rather than current live memory exactly.
**Check:** Why is `Some(empty)` different from `None`?

### Task 2: A Single Memtable in the Engine

Read [LsmStorageState::create](mini-lsm/src/lsm_storage.rs#L88) → [LsmStorageInner::put](mini-lsm/src/lsm_storage.rs#L671) →
[LsmStorageInner::write_batch](mini-lsm/src/lsm_storage.rs#L630) → [MemTable::put](mini-lsm/src/mem_table.rs#L115). Skim
[LsmStorageInner::delete](mini-lsm/src/lsm_storage.rs#L676) to see the tombstone representation.
The current solution routes single writes through the later batch API. Follow one
`Put` branch and the short `state.read()` guard around the memtable mutation;
that guard prevents a structural swap from moving this write into the wrong map.
It covers map mutation and optional WAL append, then releases at the inner block's
end, before `try_freeze`. Other writers can hold read guards concurrently; this is
not a writer-serialization mutex. See the [write sequence](LOCKING_AND_INVARIANTS.md#4-writes-and-freeze-why-dropping-the-read-guard-matters).
**Check:** Where does the public engine reject an empty ordinary value?

### Task 3: Write Path - Freezing a Memtable

Read [LsmStorageInner::try_freeze](mini-lsm/src/lsm_storage.rs#L680) → [LsmStorageInner::force_freeze_memtable](mini-lsm/src/lsm_storage.rs#L743)
→ [LsmStorageInner::freeze_memtable_with_memtable](mini-lsm/src/lsm_storage.rs#L720). The first checks capacity;
the second creates a replacement and handles its persistence metadata; the third
publishes the layout change. First understand the swap, then revisit WAL/manifest
steps in Week 2.

A freeze changes `current=A, immutable=[B]` into `current=C, immutable=[A,B]`.
It performs no SST construction. Notice the size recheck after `state_lock` and
the read guard being dropped before the write guard is acquired. The structural
mutex remains held through replacement creation, the swap, and old-WAL sync, then
releases at `try_freeze`'s outer `if` block. The helper borrows that existing guard.
**Check:** How does a second writer avoid freezing a fresh, nearly empty map?

### Task 4: Read Path - Get

Read the memory-probing portion of [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L536). Stop at the SST
candidate loop on this first pass. Current memory wins; then immutable maps are
searched newest to oldest. Return on the first occurrence, regardless of whether
it contains a value or a tombstone.

Trace current `a:2`, immutable `a:1`: return 2. Change current to `a:delete`: return
absent. **Check:** Why would continuing after that deletion return an incorrect result?

Read [test_task1_memtable_overwrite](mini-lsm/src/tests/week1_day1.rs#L45),
[test_task3_freeze_on_capacity](mini-lsm/src/tests/week1_day1.rs#L115), and
[test_task4_storage_integration](mini-lsm/src/tests/week1_day1.rs#L136).
Run: `cargo test -p mini-lsm --lib week1_day1`.

## Day 2 — Merge Iterator

Book: [Merge Iterator](mini-lsm-book/src/week1-02-merge-iterator.md#merge-iterator). Goal: expose one sorted stream across
several sorted memory maps without materializing all their entries.

### Task 1: Memtable Iterator

Read [StorageIterator](mini-lsm/src/iterators.rs#L25) → [map_bound](mini-lsm/src/mem_table.rs#L43) → [MemTable::scan](mini-lsm/src/mem_table.rs#L149) →
[MemTableIterator::entry_to_item](mini-lsm/src/mem_table.rs#L211) → [MemTableIterator::next](mini-lsm/src/mem_table.rs#L234).
The constructor owns its range bounds, retains the map through an `Arc`, and primes
the cursor once before returning. `ouroboros` manages the range iterator borrowing
the map stored inside the same structure. You can defer macro mechanics initially.
**Check:** For keys `[a,c,e]`, what is the initial key for range `(a,e]`?

### Task 2: Merge Iterator

Read [HeapWrapper::cmp](mini-lsm/src/iterators/merge_iterator.rs#L44) → [MergeIterator::create](mini-lsm/src/iterators/merge_iterator.rs#L67) → [MergeIterator::next](mini-lsm/src/iterators/merge_iterator.rs#L123).
Comparison is by key first, input index second, reversed for Rust's max-heap.
Each child must already be sorted and unique by its exposed key. Input position
supplies tie priority; read-path callers use it to encode recency. The merge itself
infers no recency from values or ids and is also used in compaction.
`current` holds the winner outside the heap; `next` consumes competing equal keys
before advancing it and choosing the next winner.

Trace input 0 `[b:9,d:4]`, input 1 `[a:1,b:2,c:3]`: output is
`[a:1,b:9,c:3,d:4]`. The older source's `a` still precedes the newer source's `b`.
**Check:** Why must priority only break ties, rather than always favor input 0?

### Task 3: LSM Iterator + Fused Iterator

Read [LsmIterator::new](mini-lsm/src/lsm_iterator.rs#L44) → [LsmIterator::move_to_non_delete](mini-lsm/src/lsm_iterator.rs#L85) →
[LsmIterator::next](mini-lsm/src/lsm_iterator.rs#L112), then [FusedIterator::is_valid](mini-lsm/src/lsm_iterator.rs#L148) → [FusedIterator::next](mini-lsm/src/lsm_iterator.rs#L167).
The first wrapper turns the raw merged stream into live user entries. The second
protects the public cursor after exhaustion or an error. An error can occur after
only some children have advanced, so it permanently taints the merged cursor.
The upper-bound logic is added in Day 5; understand deletion filtering first.
**Check:** Why are repeated calls after normal exhaustion harmless, but calls after
an error continue to fail?

### Task 4: Read Path - Scan

Read the memory-cursor construction in [LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L842), then
[LsmIteratorInner](mini-lsm/src/lsm_iterator.rs#L30) to see how the later disk sources extend that same design.
Each source handles its own starting position; the merge only compares current
entries and advances children as needed. Keep memory inputs newest first.
**Check:** Which line gives the current memtable index 0 in the merge?

Read [test_task1_memtable_iter](mini-lsm/src/tests/week1_day2.rs#L31),
[test_task2_merge_1](mini-lsm/src/tests/week1_day2.rs#L101),
[test_task2_merge_error](mini-lsm/src/tests/week1_day2.rs#L229), and
[test_task3_fused_iterator](mini-lsm/src/tests/week1_day2.rs#L256).
Run: `cargo test -p mini-lsm --lib week1_day2`.

## Day 3 — Block

Book: [Block](mini-lsm-book/src/week1-03-block.md#block). Goal: encode a sorted group of entries into a
small independently searchable unit. Builders consume sorted input; they do not sort it.

### Task 1: Block Builder

Read [BlockBuilder::new](mini-lsm/src/block/builder.rs#L51) → [BlockBuilder::add](mini-lsm/src/block/builder.rs#L67) → [BlockBuilder::build](mini-lsm/src/block/builder.rs#L127)
→ [Block::encode](mini-lsm/src/block.rs#L33) → [Block::decode_checked](mini-lsm/src/block.rs#L51).
Track two buffers: encoded entries and their u16 offsets. The encoded block ends
with the offsets and then the entry count, so decoding can locate the index from
the end. The current entry encoding already includes Day 7 prefix compression.

If a nonempty block cannot fit the next entry, `add` returns false without accepting
it; the SST builder retries in a fresh block. A first valid entry can exceed the
configured target, but key/value lengths must still fit their encoded fields.
**Check:** Why would rejecting every entry larger than the target prevent progress?

### Task 2: Block Iterator

Read [BlockIterator::create_and_seek_to_first](mini-lsm/src/block/iterator.rs#L62) → [BlockIterator::seek_to](mini-lsm/src/block/iterator.rs#L98)
→ [BlockIterator::seek_to_offset](mini-lsm/src/block/iterator.rs#L117), then [BlockIterator::seek_to_key](mini-lsm/src/block/iterator.rs#L141) and
[BlockIterator::next](mini-lsm/src/block/iterator.rs#L110). The offset array allows jumping directly to any entry.
The current key is reconstructed, while the value remains a borrowed slice of the
shared block. Seek is a lower-bound binary search, not an equality test.

For `[a,c,e]`, seeking `b` lands on `c`; seeking `z` becomes invalid.
**Check:** Which call marks the iterator invalid when the insertion position equals
the entry count?

Read [test_block_build_large_1](mini-lsm/src/tests/week1_day3.rs#L40),
[test_block_decode](mini-lsm/src/tests/week1_day3.rs#L93), and
[test_block_seek_key](mini-lsm/src/tests/week1_day3.rs#L134).
Run: `cargo test -p mini-lsm --lib week1_day3`.

## Day 4 — Sorted String Table

Book: [Sorted String Table (SST)](mini-lsm-book/src/week1-04-sst.md#sorted-string-table-sst). Goal: assemble blocks into a disk file and expose
the same cursor behavior across block boundaries.

### Task 1: SST Builder

Read [SsTableBuilder::add](mini-lsm/src/table/builder.rs#L53) → [SsTableBuilder::finish_block](mini-lsm/src/table/builder.rs#L83) →
[SsTableBuilder::build](mini-lsm/src/table/builder.rs#L100) → [BlockMeta::encode_block_meta](mini-lsm/src/table.rs#L63) →
[FileObject::create](mini-lsm/src/table.rs#L180) → [SsTable::open](mini-lsm/src/table.rs#L218).
`first_key` and `last_key` in the builder describe its current block. When that
block fills, capture its boundaries and offset, finish it, and retry the rejected
entry in a new block. `open` follows footer offsets backward and loads metadata;
it does not eagerly decode all data blocks.

Trace a boundary between blocks `[a,c]` and `[e,g]`: the first metadata entry must
end at `c`, not `e`. **Check:** Which helper transfers and clears those boundary keys?

### Task 2: SST Iterator

Read [SsTableIterator::seek_to_key_inner](mini-lsm/src/table/iterator.rs#L58) → [SsTable::find_block_idx](mini-lsm/src/table.rs#L358) →
[BlockIterator::seek_to_key](mini-lsm/src/block/iterator.rs#L141), then [SsTableIterator::next](mini-lsm/src/table/iterator.rs#L113).
The table index chooses a candidate block by first key. If seeking exhausts that
block, move to the next block's first entry. Normal iteration makes the same switch
when the current block ends.
**Check:** For blocks `[a..f]`, `[m..r]`, why does seeking `h` return `m`?

### Task 3: Block Cache

Read [SsTable::read_block_cached](mini-lsm/src/table.rs#L344) → [SsTable::read_block](mini-lsm/src/table.rs#L310) →
[FileObject::read](mini-lsm/src/table.rs#L160). Cache keys are `(table id, block index)`, so block 0 from
two different SSTs cannot collide. A hit shares a decoded `Arc<Block>`; a miss
reads and validates the encoded block. CRC handling is a Week 2 addition.
**Check:** Why would caching only by block index return data from the wrong file?

Read [test_sst_block_metadata_matches_block_contents](mini-lsm/src/tests/week1_day4.rs#L82),
[test_sst_seek_key](mini-lsm/src/tests/week1_day4.rs#L150), and
[test_sst_decode](mini-lsm/src/tests/week1_day4.rs#L101).
Run: `cargo test -p mini-lsm --lib week1_day4`.

## Day 5 — Read Path

Book: [Read Path](mini-lsm-book/src/week1-05-read-path.md#read-path). The full nine-step chat walkthrough is saved
at [READ_PATH.md](READ_PATH.md). The expanded background guide remains at
[mini-lsm/READ_PATH.md](mini-lsm/READ_PATH.md).

### Task 1: Two Merge Iterator

Read [TwoMergeIterator::create](mini-lsm/src/iterators/two_merge_iterator.rs#L69) → [TwoMergeIterator::skip_b](mini-lsm/src/iterators/two_merge_iterator.rs#L52) →
[TwoMergeIterator::choose_a](mini-lsm/src/iterators/two_merge_iterator.rs#L39) → [TwoMergeIterator::next](mini-lsm/src/iterators/two_merge_iterator.rs#L115). A and B may have
different cursor types but comparable key types. Equal keys select A because B's
copy is advanced first. This applies to ordinary overwrites and deletes alike.
Each child must be sorted and individually unique by that key type; otherwise one
B advance need not remove the conflict. Consumers decide what to do with a winning
tombstone: a scan hides it, while compaction may need to retain it.
**Check:** Merge A=`[b:9,d:4]`, B=`[b:2,c:3]`; why can `choose_a` use strict `<`?

### Task 2: Read Path - Scan

Read [LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L842) → [range_overlap](mini-lsm/src/lsm_storage.rs#L158) → [LsmIterator::new](mini-lsm/src/lsm_iterator.rs#L44)
→ [LsmIterator::check_end_bound](mini-lsm/src/lsm_iterator.rs#L59) → [LsmIterator::move_to_non_delete](mini-lsm/src/lsm_iterator.rs#L85).
Create each source cursor at the lower bound, merge with memory preferred over
disk, enforce the upper bound, then expose live entries. The constructor checks
bounds too: seeking `[b,b]` in an SST `[a,c]` already lands out of range.
**Check:** Why must an excluded lower bound advance only when the seek is exactly equal?

### Task 3: Read Path - Get

Read [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L536) → [MemTable::get](mini-lsm/src/mem_table.rs#L106) →
[SsTableIterator::create_and_seek_to_key](mini-lsm/src/table/iterator.rs#L79). Memory returns on its first hit;
disk sources are sought and merged. The final comparison must establish exact key
equality as well as a nonempty value. Bounds/Bloom filtering only prune candidates.
**Check:** Where does a valid seek to `c` get rejected for `get(b)`?

Read [test_task2_storage_scan](mini-lsm/src/tests/week1_day5.rs#L146),
[test_task2_storage_scan_end_bound_at_seek_position](mini-lsm/src/tests/week1_day5.rs#L226), and
[test_task3_storage_get](mini-lsm/src/tests/week1_day5.rs#L269).
Run: `cargo test -p mini-lsm --lib week1_day5`.

## Day 6 — Write Path

Book: [Write Path](mini-lsm-book/src/week1-06-write-path.md#write-path). Goal: replace an immutable memory source
with an equivalent SST and arrange for this to happen in the background.

### Task 1: Flush Memtable to SST

Read [MemTable::flush](mini-lsm/src/mem_table.rs#L167) → [LsmStorageInner::force_flush_next_imm_memtable](mini-lsm/src/lsm_storage.rs#L773)
→ [SsTableBuilder::build](mini-lsm/src/table/builder.rs#L100). The oldest immutable map is written in sorted order,
including deletion markers. After the file is ready, the new state removes that
memory source and installs the SST. WAL deletion and manifest updates are later
persistence additions; they explain the additional steps in today's solution.

For immutable maps `[newer=A, older=B]`, flush B first and leave A in memory, so A
still wins equal keys. **Check:** Why is B's newly flushed SST inserted at the front
of L0 even though B is the oldest immutable map?

### Task 2: Flush Trigger

Read [LsmStorageInner::try_freeze](mini-lsm/src/lsm_storage.rs#L680) → [LsmStorageInner::trigger_flush](mini-lsm/src/compact.rs#L444) →
[LsmStorageInner::spawn_flush_thread](mini-lsm/src/compact.rs#L459) → [MiniLsm::open](mini-lsm/src/lsm_storage.rs#L293). Capacity rotates
the current memtable; immutable-map count triggers flushing. These are separate
thresholds. The worker checks periodically and flushes one map per trigger. Flush holds
`state_lock` for its whole call, but its selection read guard ends before building
the SST and its installation write guard ends before recording the manifest.
See the [flush sequence](LOCKING_AND_INVARIANTS.md#5-flush-and-compaction-have-different-lock-lifetimes).
**Check:** Which lock stays held across flush construction, and which state lock is
released before the expensive SST build?

### Task 3: Filter the SSTs

Read [range_overlap](mini-lsm/src/lsm_storage.rs#L158) and [key_within](mini-lsm/src/lsm_storage.rs#L188), then their use in
[LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L842) and [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L536). An SST's first/last keys
allow rejecting it before creating a cursor or loading a block. This is only a
candidate test: the table can contain gaps inside its overall range.

For SST `[c,f]`, scan `[a,c)` cannot overlap, while `[a,c]` can.
**Check:** Why do included and excluded endpoints use different comparisons?

Read [test_task1_storage_scan](mini-lsm/src/tests/week1_day6.rs#L42),
[test_task2_auto_flush](mini-lsm/src/tests/week1_day6.rs#L148), and
[test_task3_sst_filter](mini-lsm/src/tests/week1_day6.rs#L167).
Run: `cargo test -p mini-lsm --lib week1_day6`.

## Day 7 — SST Optimizations

Book: [Snack Time: SST Optimizations](mini-lsm-book/src/week1-07-sst-optimizations.md#snack-time-sst-optimizations). Goal: reduce disk work and encoded
key size while preserving exactly the same logical read results.

### Task 1: Bloom Filters

Read [Bloom::bloom_bits_per_key](mini-lsm/src/table/bloom.rs#L93) → [Bloom::build_from_key_hashes](mini-lsm/src/table/bloom.rs#L100) →
[Bloom::may_contain](mini-lsm/src/table/bloom.rs#L126). Construction sets several bits per key; lookup repeats
the same probes. One zero bit proves absence; all ones only say possible presence.
The rotated hash supplies a delta for successive probe positions.
**Check:** Can two different keys setting the same bits cause a missing real key,
or only an unnecessary table read?

### Task 2: Integrate Bloom Filter on the Read Path

Read [SsTableBuilder::add](mini-lsm/src/table/builder.rs#L53) → [SsTableBuilder::build](mini-lsm/src/table/builder.rs#L100) → [SsTable::open](mini-lsm/src/table.rs#L218)
→ [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L536). Collect fingerprints during building, encode the
filter in the file, recover it when opening, then consult it before seeking.
Tombstone keys must be included too: otherwise skipping a table could resurrect
an older value. Arbitrary range scans cannot be rejected with a point-membership probe.
**Check:** Why must a positive Bloom result still go through exact key comparison?

### Task 3: Key Prefix Encoding + Decoding

Read [compute_overlap](mini-lsm/src/block/builder.rs#L33) → [BlockBuilder::add](mini-lsm/src/block/builder.rs#L67) →
[BlockIterator::seek_to_offset](mini-lsm/src/block/iterator.rs#L117). Prefixes refer to the block's **first key**,
allowing each later entry to be decoded independently. For first key `apple`,
`apply` stores overlap 4 plus suffix `y`. Length fields and offset calculations
must reflect the encoded suffix, not the reconstructed key length.
**Check:** Why would a previous-entry prefix complicate a random binary-search probe?

Read [test_task1_bloom_filter](mini-lsm/src/tests/week1_day7.rs#L35),
[test_task2_sst_decode](mini-lsm/src/tests/week1_day7.rs#L64), and
[test_task3_block_key_compression](mini-lsm/src/tests/week1_day7.rs#L82).
Run: `cargo test -p mini-lsm --lib week1_day7`.

## End-of-week trace

Start with `a=1`, freeze, write `a=2,b=3`, freeze again, delete `b`, then flush the
oldest maps. Predict `get(a)` and a full scan after each step. The locations change;
the winning logical entries should not. Use the builders to explain the disk
representation and the merge tree to explain the read results.

Bonus exercises in the book (alternative iterators, parallel seek, alternative
encodings) are optional design work, not additional completed implementations in
this reference. Use the book's bonus sections after the core tasks above.
