# Read path — annotated walkthrough saved from chat

This preserves the nine-step reading order from the chat, with the clarification
that duplicate resolution applies to ordinary values as well as tombstones. Every
function link opens the corresponding annotated code in this checkout.
The longer companion is [mini-lsm/READ_PATH.md](mini-lsm/READ_PATH.md).

For exact guard lifetimes and reusable contracts, read
[locks, ownership, and guarantees](LOCKING_AND_INVARIANTS.md).

## 1. Understand the sources a read searches

Start with [LsmStorageState](mini-lsm/src/lsm_storage.rs#L49). Follow the fields: current memtable → immutable
memtables → L0 SSTs → levels.

The order matters when several sources contain the same key. The `sstables` map
only looks up table objects; the vectors determine priority. Distinguish a snapshot
of this source layout from a snapshot of actual values: the current memtable can
still change. Week 3 later introduces timestamped snapshots.
In both `get` and `scan`, `state.read()` acquires the layout read lock, the block
clones its Arc, and the closing brace releases the guard. Memory/disk reads and
returned scan iteration then proceed without retaining that state guard. The Arc
keeps objects alive; it does not keep a lock acquired.

## 2. Understand how one memory source is read

Read [StorageIterator](mini-lsm/src/iterators.rs#L25), then [MemTable::get](mini-lsm/src/mem_table.rs#L106), [MemTable::scan](mini-lsm/src/mem_table.rs#L149), and
[MemTableIterator::next](mini-lsm/src/mem_table.rs#L234).

A cursor is already positioned when returned: check `is_valid()`, read its key and
value, then advance. An empty **value** represents deletion; an empty **key** marks
this memory cursor as exhausted. `None` means this map has no entry and permits
searching older sources; `Some(empty)` is an authoritative deletion.

## 3. Understand merging several sources of the same type

Read [HeapWrapper::cmp](mini-lsm/src/iterators/merge_iterator.rs#L44) → [MergeIterator::create](mini-lsm/src/iterators/merge_iterator.rs#L67) → [MergeIterator::next](mini-lsm/src/iterators/merge_iterator.rs#L123).

Focus on two separate rules: **smaller keys come first; source priority breaks
equal-key ties**. The comments explain the reversed heap comparison and trace how
`next()` consumes competing copies before advancing the winning entry.

For input 0 `[b:9,d:4]` and input 1 `[a:1,b:2,c:3]`, output is
`[a:1,b:9,c:3,d:4]`. Input 0 wins b, but input 1's smaller a still comes first.

## 4. Book Task 1: merge memory and disk cursors

Read [TwoMergeIterator::create](mini-lsm/src/iterators/two_merge_iterator.rs#L69) → [TwoMergeIterator::skip_b](mini-lsm/src/iterators/two_merge_iterator.rs#L52) →
[TwoMergeIterator::choose_a](mini-lsm/src/iterators/two_merge_iterator.rs#L39) → [TwoMergeIterator::next](mini-lsm/src/iterators/two_merge_iterator.rs#L115).

A and B can be different cursor types. When their keys are equal, A wins because
the caller supplied it as the higher-priority input. `skip_b` advances B past its
copy so the merged stream will emit the key only once. It does not inspect values
or decide source recency itself. Both children must already be sorted and unique
by their exposed key; that precondition is why advancing B once is sufficient.
The same primitive merges compaction inputs. In Week 3, raw merges compare full
`(user key, timestamp)` keys, while transaction overlays compare user-key bytes.

Trace A=`[b:9,d:4]`, B=`[b:2,c:3]`: B advances to c while A remains at b; the output
is `[b:9,c:3,d:4]`. This is the general rule for an overwrite.

Now replace A's b value with a tombstone. The same merge rule produces
`[b:delete,c:3,d:4]`. A later wrapper hides the winning deletion after B's old b
has been suppressed. The full pipeline is **key order → duplicate priority →
visibility filtering**. Deletion does not define the merge algorithm.

## 5. Understand what seeking an SST actually does

Follow [SsTableIterator::seek_to_key_inner](mini-lsm/src/table/iterator.rs#L58) → [SsTable::find_block_idx](mini-lsm/src/table.rs#L358) →
[SsTable::read_block_cached](mini-lsm/src/table.rs#L344) → [BlockIterator::seek_to_key](mini-lsm/src/block/iterator.rs#L141).

The first search chooses a block; the second searches entries inside it. The
comments trace seeking h between blocks `[a..f]` and `[m..r]`: the result is m.
**A valid seek result does not necessarily mean the requested key exists.**

Read [BlockIterator::seek_to_offset](mini-lsm/src/block/iterator.rs#L117) for key reconstruction and
[SsTableIterator::next](mini-lsm/src/table/iterator.rs#L113) for crossing block boundaries. Prefix compression
uses the block's first key, so a binary-search probe can decode an entry directly.

## 6. Book Task 2: assemble the scan

Start at [LsmStorageInner::scan](mini-lsm/src/lsm_storage.rs#L842). Read its annotated stages in order:

1. [Capture the source layout](mini-lsm/src/lsm_storage.rs#L850).
2. [Build memory cursors](mini-lsm/src/lsm_storage.rs#L857).
3. [Build L0 cursors](mini-lsm/src/lsm_storage.rs#L868).
4. [Build level/tier cursors](mini-lsm/src/lsm_storage.rs#L905).
5. [Merge source groups](mini-lsm/src/lsm_storage.rs#L942).
6. [Wrap the result](mini-lsm/src/lsm_storage.rs#L947).

Pay attention to the excluded lower bound: if seeking b lands on b, advance; if
it lands on c, keep c. [range_overlap](mini-lsm/src/lsm_storage.rs#L158) only selects possible tables; it does
not enforce the exact cursor bounds. The scan builds cursors and advances them on
demand, rather than collecting all results at construction.

## 7. Finish Task 2: turn raw entries into visible results

Read [LsmIterator::new](mini-lsm/src/lsm_iterator.rs#L44), [LsmIterator::check_end_bound](mini-lsm/src/lsm_iterator.rs#L59),
[LsmIterator::move_to_non_delete](mini-lsm/src/lsm_iterator.rs#L85), and [LsmIterator::next](mini-lsm/src/lsm_iterator.rs#L112).

These enforce the upper bound and hide winning tombstones **after merging has
resolved duplicates**. The constructor checks the bound immediately: scan `[b,b]`
over `[a,c]` must start invalid. Skipping consecutive deleted keys needs a loop,
and every extra raw step must check exhaustion and the upper bound too.

Finish with [FusedIterator::next](mini-lsm/src/lsm_iterator.rs#L167). Normal exhaustion permits harmless repeated
calls; an iteration error permanently invalidates the cursor because only some
children may have advanced before that failure.

## 8. Book Task 3: follow a point lookup end to end

Read [LsmStorageInner::get](mini-lsm/src/lsm_storage.rs#L536), particularly the
[candidate-table filter](mini-lsm/src/lsm_storage.rs#L568) and
[final result check](mini-lsm/src/lsm_storage.rs#L618).

Memory is checked directly, newest first. A value or a tombstone ends the lookup.
Disk candidates are sought and merged, then the final condition requires a valid
cursor, **exact key equality**, and a nonempty value. Bloom filters only prune
impossible candidates; positive membership is not proof of a match.

A disk result is copied into owned `Bytes`, so it remains valid when the temporary
cursor and its borrowed block value go away. Memory results instead clone a `Bytes`
handle, sharing its already-owned byte buffer.

## 9. Read the annotated tests as complete examples

Start with [test_task2_storage_scan](mini-lsm/src/tests/week1_day5.rs#L146), then
[test_task2_storage_scan_end_bound_at_seek_position](mini-lsm/src/tests/week1_day5.rs#L226), and
[test_task3_storage_get](mini-lsm/src/tests/week1_day5.rs#L269). The comments identify which
source wins and what output to predict before reading the assertions.

For one complete trace, assume no concurrent writes:

```text
current memory:   b → delete, d → 4
immutable memory: a → 1, b → 2
newest L0 SST:     a → 0, c → 3, d → 3
```

`get(a)` returns 1; `get(b)` is absent; scan `[a,d]` returns `[a:1,c:3,d:4]`.
Changing the bounds to `(a,d)` leaves `[c:3]`. Trace the raw b tombstone through
merging before the visibility wrapper suppresses it.

Run from the repository root:

```sh
cargo test -p mini-lsm --lib week1_day5
```

The Week 2 extensions are [SstConcatIterator::create_and_seek_to_key](mini-lsm/src/iterators/concat_iterator.rs#L68) and
[Bloom::may_contain](mini-lsm/src/table/bloom.rs#L126). The full course continues in the root week-by-week guides.
