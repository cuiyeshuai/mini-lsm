# Mini-LSM — annotated code reading roadmap

Start here. The read-path chat walkthrough is saved in
[READ_PATH.md](READ_PATH.md), with clickable links to exact annotated functions
and to individual stages inside `get` and `scan`.

The guides below cover every numbered implementation task in the book's **21
chapters across Weeks 1–3**, including the tiered/leveled compaction subtasks.
Each task gives a reading order, explains each function's role in the larger flow,
and includes a trace or checkpoint. Each chapter points to existing tests.

| Week | Guide | Code to read | Overall result |
| --- | --- | --- | --- |
| 1 | [Week 1 annotated guide](WEEK1_ROADMAP.md) | `mini-lsm/` | Sorted memory, encoded disk files, one logical read/write path |
| 2 | [Week 2 annotated guide](WEEK2_ROADMAP.md) | `mini-lsm/` | Compaction scheduling, installation, and recovery |
| 3 | [Week 3 annotated guide](WEEK3_ROADMAP.md) | `mini-lsm-mvcc/` | Timestamped versions, snapshots, GC, and transactions |

## Jump to a chapter

| Day | Week 1 | Week 2 | Week 3 |
| --- | --- | --- | --- |
| 1 | [Memtables](WEEK1_ROADMAP.md#day-1--memtables) | [Compaction implementation](WEEK2_ROADMAP.md#day-1--compaction-implementation) | [Timestamp encoding](WEEK3_ROADMAP.md#day-1--timestamp-key-encoding--refactor) |
| 2 | [Merge iterators](WEEK1_ROADMAP.md#day-2--merge-iterator) | [Simple compaction](WEEK2_ROADMAP.md#day-2--simple-compaction-strategy) | [Memtables and timestamps](WEEK3_ROADMAP.md#day-2--snapshots-memtables-and-timestamps) |
| 3 | [Blocks](WEEK1_ROADMAP.md#day-3--block) | [Tiered compaction](WEEK2_ROADMAP.md#day-3--tiered-compaction-strategy) | [Snapshot read API](WEEK3_ROADMAP.md#day-3--snapshots-engine-read-path-and-transaction-api) |
| 4 | [SSTs and cache](WEEK1_ROADMAP.md#day-4--sorted-string-table) | [Leveled compaction](WEEK2_ROADMAP.md#day-4--leveled-compaction-strategy) | [Watermark and GC](WEEK3_ROADMAP.md#day-4--watermark-and-garbage-collection) |
| 5 | [Read path](READ_PATH.md) | [Manifest](WEEK2_ROADMAP.md#day-5--manifest) | [Atomic transactions](WEEK3_ROADMAP.md#day-5--transaction-workspace-and-atomic-commit) |
| 6 | [Write path](WEEK1_ROADMAP.md#day-6--write-path) | [WAL](WEEK2_ROADMAP.md#day-6--write-ahead-log) | [Serializable validation](WEEK3_ROADMAP.md#day-6--serializable-validation) |
| 7 | [Bloom filters and prefix encoding](WEEK1_ROADMAP.md#day-7--sst-optimizations) | [Batches and checksums](WEEK2_ROADMAP.md#day-7--batch-write-and-checksums) | [Compaction filters](WEEK3_ROADMAP.md#day-7--compaction-filters) |

## How to read the annotated code

1. Read the task's purpose and follow its function links in the listed order.
2. Use the inline comments to track state changes and cursor positions. Helpers
   are linked where their behavior matters; you need not read whole files linearly.
3. Predict the small example's output before inspecting the corresponding test.
4. Answer the checkpoint in terms of the invariant being preserved, not merely
   what each line does. For a merge, separate key ordering from duplicate priority;
   for persistence, separate writing bytes from making a layout recoverable.
5. Revisit the end-of-week trace to connect the individual pieces.

These are completed reference solutions. A function can include later lessons:
Week 1 code already contains checksums and WAL hooks, and Week 3 code already
contains final GC and validation behavior. The guides identify these additions.
Some scaffold comments in the original code still refer to future implementation
steps; the linked function body shows what this checkout actually implements.

The book's open-ended bonus tasks and Week 4 project ideas do not all have completed
reference implementations. Their book sections remain the starting points; this
index does not invent code links or claim those optional projects are implemented.

## Link behavior and validation

Source links use repository-relative paths with GitHub line anchors, and chapter
links jump to the corresponding Markdown headings. Open the rendered guides on
GitHub to follow them. The links stay within this fork and also work when viewing
another branch. Subsequent source edits can shift line numbers; function names
remain stable navigation references.

All code edits for these guides are explanatory comments. Existing test commands:

```sh
cargo test -p mini-lsm --lib
cargo test -p mini-lsm-mvcc --lib
```

Chapter guides also include narrower commands. Week 2 Day 7 uses existing corruption
regressions because this solution has no separate `week2_day7.rs` test module.
