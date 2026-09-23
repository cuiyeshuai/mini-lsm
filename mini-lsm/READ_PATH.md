# Reading the Mini-LSM read path

This guide follows the implementation in **`mini-lsm/`**, the Week 1 + Week 2
solution. Start with the book's [Read Path chapter](../mini-lsm-book/src/week1-05-read-path.md).
The code here already includes later additions: Bloom filters, compressed block
keys, and reads from compacted levels/tiers. Those are marked below so you can
separate the chapter's core ideas from the extensions.

Read the sections below in order. Each named function links directly to its
annotated source line in this checkout. Within `get` and `scan`, the numbered
steps also link to the corresponding code sections. The comments include small
input/output traces so you can follow cursor positions alongside the code.

These source links reflect the current annotated files; function names remain
the navigation reference if later edits move the code.

For exact guard lifetimes and reusable contracts, read
[locks, ownership, and guarantees](../LOCKING_AND_INVARIANTS.md).

## 0. Understand what a read must produce

Read **[LsmStorageState](src/lsm_storage.rs#L49)** in [lsm_storage.rs](src/lsm_storage.rs), then skim
**[MiniLsm::get](src/lsm_storage.rs#L312)** and **[MiniLsm::scan](src/lsm_storage.rs#L339)**. These public methods delegate to
`LsmStorageInner`, where the work happens.

An SST is an immutable, sorted table on disk. A memtable is the in-memory sorted
map receiving writes; frozen memtables wait to be flushed into SSTs. Several
sources can contain the same key, so reads must resolve which entry wins:

| Source | How duplicate-key priority is determined |
| --- | --- |
| Current memtable | Highest priority |
| Immutable memtables | Earlier vector entry wins; newest is first |
| L0 SSTs | Earlier vector entry wins; newest is first |
| Levels or tiers | Earlier level/tier wins; SSTs within each one have disjoint key ranges |

For leveled compaction, the last row means L1 before L2 and so on. For tiered
compaction, it means newer tiers before older tiers. The `sstables` map resolves
IDs to table objects; its iteration order does not determine priority.

Keep these rules in mind:

- An **empty value is a tombstone**: the key was deleted. It must hide older values.
- `get(k)` returns the highest-priority entry only if its key equals `k` and it is live.
- `scan(lower, upper)` returns sorted, unique, live entries within the bounds.
- A seek returns the first key **greater than or equal to** its target. It can miss
  the target while still producing a valid cursor.
- Cloning the state `Arc` pins a consistent source layout and releases the state
  lock before disk I/O. It does **not** provide a point-in-time MVCC snapshot:
  the current memtable remains shared and writable. `state.read()` acquires a
  guard inside the snapshot block; its closing brace releases it. Keeping the Arc
  does not keep that guard or acquire the separate structural `state_lock`.

## 1. Learn the cursor contract and memory source

Read these in order:

1. [iterators.rs](src/iterators.rs): **[StorageIterator](src/iterators.rs#L25)**, especially `is_valid`,
   `key`, `value`, and `next`.
2. [mem_table.rs](src/mem_table.rs): **[MemTable::get](src/mem_table.rs#L106)**, **[map_bound](src/mem_table.rs#L43)**,
   **[MemTable::scan](src/mem_table.rs#L149)**.
3. The same file: **[MemTableIterator::entry_to_item](src/mem_table.rs#L211)**, **[MemTableIterator::is_valid](src/mem_table.rs#L229)**, **[MemTableIterator::next](src/mem_table.rs#L234)**
   in its `StorageIterator` implementation.

These iterators are cursors: construction already positions them at the first
candidate. Read the current entry before advancing. `MemTable::scan` calls
`next()` internally to establish that initial position, and the skipmap handles
both range bounds.

Distinguish three cases: no entry, an entry with an empty value, and a live entry.
`MemTable::get` returns `None`, `Some(empty)`, and `Some(value)` respectively.
The memory cursor uses an empty **key** to represent exhaustion; an empty
**value** is still a valid entry.

For a first pass, just understand that `MemTableIterator` owns an `Arc` to the map
and a range iterator borrowing that map. You can defer the `ouroboros` macro details.

**Checkpoint:** Why must `Some(empty)` stop a point lookup instead of letting it
continue into an older memtable?

## 2. Resolve duplicates — book Task 1: Two Merge Iterator

First revisit the prerequisite in [merge_iterator.rs](src/iterators/merge_iterator.rs):

1. **[HeapWrapper::cmp](src/iterators/merge_iterator.rs#L44)** — sorts by key, then original input index. The comparison
   is reversed because Rust's `BinaryHeap` puts the maximum at the top.
2. **[MergeIterator::create](src/iterators/merge_iterator.rs#L67)** — keeps the winning cursor as `current` and the
   competing cursors in a heap.
3. **[MergeIterator::next](src/iterators/merge_iterator.rs#L123)** — advances competing copies of the current key,
   advances the winner, and selects the next smallest key.

This merges many cursors of the same type. The smaller input index wins ties,
which is why the engine builds input vectors in source-priority order. Each child
must already be sorted and individually unique by its exposed key type.

Then read [two_merge_iterator.rs](src/iterators/two_merge_iterator.rs):

1. **[TwoMergeIterator::create](src/iterators/two_merge_iterator.rs#L69)**
2. **[TwoMergeIterator::skip_b](src/iterators/two_merge_iterator.rs#L52)**
3. **[TwoMergeIterator::choose_a](src/iterators/two_merge_iterator.rs#L39)**
4. **[TwoMergeIterator::next](src/iterators/two_merge_iterator.rs#L115)**, then skim **[TwoMergeIterator::key](src/iterators/two_merge_iterator.rs#L91)**, **[TwoMergeIterator::value](src/iterators/two_merge_iterator.rs#L99)**, **[TwoMergeIterator::is_valid](src/iterators/two_merge_iterator.rs#L107)**

This merges two cursor types, such as memory and SST cursors. A wins equal keys:
`skip_b` advances B past the duplicate before `choose_a` compares the remaining
keys. This explains why `choose_a` can use `<` rather than `<=`.

Both merge layers preserve tombstones. They resolve duplicate keys without
deciding whether the winning entry should be visible to the caller. Compaction
also uses these merges; its consumer may retain a winning tombstone on disk.
In Week 3 raw merges compare full `(user key, timestamp)` keys, so different
versions survive; transaction overlays instead merge by user-key bytes.

**Checkpoint:** First merge A = `[b:9, d:4]` with B = `[b:2, c:3]`: the
output is `[b:9, c:3, d:4]`. This is ordinary duplicate resolution. Now replace
A's b with a tombstone: the raw output is `[b:delete, c:3, d:4]`, and only a
later wrapper removes b. `skip_b` never inspects either value.

## 3. Follow a seek down to disk

You only need the read-side functions from the earlier block/SST chapters:

1. [table/iterator.rs](src/table/iterator.rs):
   **[SsTableIterator::create_and_seek_to_key](src/table/iterator.rs#L79)** → **[SsTableIterator::seek_to_key_inner](src/table/iterator.rs#L58)**.
2. [table.rs](src/table.rs): **[SsTable::find_block_idx](src/table.rs#L358)** →
   **[SsTable::read_block_cached](src/table.rs#L344)** → **[SsTable::read_block](src/table.rs#L310)** → **[FileObject::read](src/table.rs#L160)**.
3. [block/iterator.rs](src/block/iterator.rs):
   **[BlockIterator::create_and_seek_to_key](src/block/iterator.rs#L69)** → **[BlockIterator::seek_to_key](src/block/iterator.rs#L141)** →
   **[BlockIterator::seek_to](src/block/iterator.rs#L98)** → **[BlockIterator::seek_to_offset](src/block/iterator.rs#L117)**.
4. Return to **[SsTableIterator::next](src/table/iterator.rs#L113)** to see how iteration crosses block boundaries.
   Skim **[SsTableIterator::create_and_seek_to_first](src/table/iterator.rs#L40)** and **[SsTableIterator::seek_to_first_inner](src/table/iterator.rs#L32)** for unbounded scans.

The table first searches its in-memory block metadata. It loads the candidate
block through the cache, then binary-searches entries using the block's offset
array. `read_block` checks the checksum and calls `Block::decode_checked`; you can
defer the decoder's format-validation details until you study the file format.

If the target falls between two blocks, seeking in the earlier block exhausts it;
`seek_to_key_inner` then moves to the next block's first entry. Seeking past the
table's final key leaves the cursor invalid.

The current implementation includes prefix compression: `seek_to_offset`
reconstructs a key from a prefix of the block's **first key** plus the entry's
stored suffix. That makes each entry independently decodable during binary search.

**Checkpoint:** A table contains `a` and `c`. Seeking `b` produces `c`, so what
additional condition must `get(b)` check?

## 4. Assemble a range scan — book Task 2: Read Path - Scan

Read **[LsmStorageInner::scan](src/lsm_storage.rs#L842)** in [lsm_storage.rs](src/lsm_storage.rs).
The numbered comments follow its construction steps:

1. [Clone the source layout](src/lsm_storage.rs#L850) under the read lock, then release the lock.
2. [Create bounded memory cursors](src/lsm_storage.rs#L857) in priority order and merge them.
3. [Build L0 cursors](src/lsm_storage.rs#L868): use **[range_overlap](src/lsm_storage.rs#L158)** to skip irrelevant L0 tables, seek the remaining tables
   to the lower bound, and merge their cursors.
4. [Build level/tier cursors](src/lsm_storage.rs#L905). On the first pass, treat these as one more sorted
   source group; section 6 explains their construction.
5. [Merge the groups](src/lsm_storage.rs#L942) with memory preferred over L0, and L0 over levels/tiers.
6. [Wrap the result](src/lsm_storage.rs#L947) in `LsmIterator`, then `FusedIterator`.

Read [lsm_iterator.rs](src/lsm_iterator.rs) next:

1. **[LsmIteratorInner](src/lsm_iterator.rs#L30)** — match the nested type to the merges just constructed.
2. **[LsmIterator::new](src/lsm_iterator.rs#L44)** → **[LsmIterator::check_end_bound](src/lsm_iterator.rs#L59)** → **[LsmIterator::move_to_non_delete](src/lsm_iterator.rs#L85)**.
3. **[LsmIterator::next](src/lsm_iterator.rs#L112)** → **[LsmIterator::next_inner](src/lsm_iterator.rs#L72)**, then back to **[LsmIterator::move_to_non_delete](src/lsm_iterator.rs#L85)**.
4. **[FusedIterator::is_valid](src/lsm_iterator.rs#L148)** and **[FusedIterator::next](src/lsm_iterator.rs#L167)** — exhausted cursors stay exhausted;
   an iteration error makes subsequent `next()` calls fail too.

The scan pipeline is:

```text
memory cursors ── MergeIterator ──┐
                                ├─ TwoMergeIterator ──┐
L0 SST cursors ── MergeIterator ──┘                     ├─ TwoMergeIterator
level/tier concat cursors ──────── MergeIterator ────────┘
    → LsmIterator: upper bound + tombstone removal
    → FusedIterator: exhaustion/error handling
```

Pay particular attention to **where bounds are enforced**:

| Bound | Memory | SST sources |
| --- | --- | --- |
| Included lower | Skipmap range | Seek to first key >= lower |
| Excluded lower | Skipmap range | Seek to >= lower, then advance if exactly equal |
| Upper | Skipmap range | `LsmIterator::check_end_bound`: `<=` for included, `<` for excluded |

`range_overlap` only selects possible tables; it does not replace these cursor
checks. `LsmIterator::new` must check the upper bound immediately, since the
initial seek can already land beyond it. Every advance while skipping tombstones
must check the bound too.

**Checkpoint:** Why would filtering tombstones out of individual source cursors
*before* merging make a deleted key reappear?

## 5. Follow a point lookup — book Task 3: Read Path - Get

Read **[LsmStorageInner::get](src/lsm_storage.rs#L536)** in [lsm_storage.rs](src/lsm_storage.rs), following
its numbered comments:

1. [Clone the layout](src/lsm_storage.rs#L537) and release the state lock.
2. [Probe memory](src/lsm_storage.rs#L545): call `MemTable::get` on current, then immutable memtables. Return immediately
   on the first occurrence, including a tombstone.
3. Inspect the **[keep_table closure](src/lsm_storage.rs#L568)** and **[key_within](src/lsm_storage.rs#L188)**. For candidate SSTs,
   [Seek and merge L0 tables](src/lsm_storage.rs#L588), then [build the level/tier cursors](src/lsm_storage.rs#L600) and merge the disk sources in priority order.
4. [Inspect the final condition](src/lsm_storage.rs#L618): cursor valid **and key exactly equal and value
   nonempty**. Copy the winning value into owned `Bytes`, or return `None`.

The Bloom-filter check is a later optimization. For the core chapter, think of
`keep_table` as rejecting tables that cannot contain the requested key. For the
details, read **[Bloom::may_contain](src/table/bloom.rs#L126)** in [table/bloom.rs](src/table/bloom.rs):
`false` rules the key out, while `true` means it might exist. The exact comparison
in `get` remains necessary. Range scans cannot use this point-membership test to
rule out a whole arbitrary range.

Unlike `scan`, `get` probes memory directly and does not build memory merge
cursors or use `LsmIterator`. It handles the final tombstone check itself.

**Checkpoint:** Find both places a tombstone can end this lookup: during memory
probing and after the disk merge.

## 6. Understand the Week 2 extension: concat within a level

Read the book's [Compaction Implementation](../mini-lsm-book/src/week2-01-compaction.md),
specifically **Task 2: Concat Iterator** and **Task 3: Integrate with the Read Path**.
Then read [concat_iterator.rs](src/iterators/concat_iterator.rs):

1. **[SstConcatIterator::check_sst_valid](src/iterators/concat_iterator.rs#L35)** — SST ranges must be sorted and disjoint.
2. **[SstConcatIterator::create_and_seek_to_key](src/iterators/concat_iterator.rs#L68)** — choose a candidate table by its first key.
3. **[SstConcatIterator::move_until_valid](src/iterators/concat_iterator.rs#L94)** — advance to another table when the current one is exhausted.
4. **[SstConcatIterator::next](src/iterators/concat_iterator.rs#L133)**, then skim **[SstConcatIterator::create_and_seek_to_first](src/iterators/concat_iterator.rs#L48)**.

Within one level/tier, concatenation is sufficient: all keys in one table precede
all keys in the next. It needs only one active SST cursor. Across levels/tiers,
keys can overlap, so `MergeIterator` is still required. Return to the
`level_iters` loops in `get` and `scan` to connect this to the full read path.

**Checkpoint:** Which assertion prevents this iterator from being used for
arbitrary overlapping L0 SSTs?

## 7. Use the tests as worked examples

Read the setup first, predict the output, then inspect the assertions:

| Test file | Functions to read | What to trace |
| --- | --- | --- |
| [week1_day5.rs](src/tests/week1_day5.rs) | [test_task1_merge_1](src/tests/week1_day5.rs#L30), [test_task1_merge_2](src/tests/week1_day5.rs#L55), [test_task1_merge_4](src/tests/week1_day5.rs#L104), [test_task1_merge_5](src/tests/week1_day5.rs#L138) | A wins duplicates; one or both inputs may be empty |
| Same file | [test_task2_storage_scan](src/tests/week1_day5.rs#L146) | Memory/L0 precedence, tombstones, included/excluded bounds |
| Same file | [test_task2_storage_scan_end_bound_at_seek_position](src/tests/week1_day5.rs#L226) | The first seek result can already exceed the upper bound |
| Same file | [test_task3_storage_get](src/tests/week1_day5.rs#L269) | Point reads across memory and L0, deletion and absence |
| [week1_day2.rs](src/tests/week1_day2.rs) | [test_task2_merge_1](src/tests/week1_day2.rs#L101), [test_task2_merge_error](src/tests/week1_day2.rs#L229), [test_task3_fused_iterator](src/tests/week1_day2.rs#L256) | Heap merge and error handling |
| [week1_day4.rs](src/tests/week1_day4.rs) | [test_sst_seek_key](src/tests/week1_day4.rs#L150) | Seeking within and beyond table contents |
| [week2_day1.rs](src/tests/week2_day1.rs) | [test_task2_concat_iterator](src/tests/week2_day1.rs#L188), [test_task3_integration](src/tests/week2_day1.rs#L224) | Crossing SST boundaries and reading compacted data |

Run the chapter tests from the repository root:

```sh
cargo test -p mini-lsm --lib week1_day5
```

For one complete mental trace, assume no concurrent writes and these sources:

```text
current memtable:  b → delete, d → 4
immutable table:   a → 1, b → 2
newest L0 SST:     a → 0, c → 3, d → 3
```

`get(a)` is `1`, `get(b)` is absent, and an inclusive scan from `a` through `d`
returns `[a:1, c:3, d:4]`. Trace `b` through merging before its tombstone is
removed. Then change the bounds to `(a, d)` and check that only `[c:3]` remains.

You can defer write batching, WAL, recovery, SST builders, compaction scheduling,
and the `mini-lsm-mvcc/` implementation until this read flow is clear.
