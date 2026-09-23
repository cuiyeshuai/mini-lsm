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

use anyhow::Result;

use super::StorageIterator;

/// Merges two iterators of different types into one. If the two iterators have the same key, only
/// produce the key once and prefer the entry from A.
///
/// Contract: each child is positioned, sorted, and unique by its exposed key
/// type. Source priority comes from the caller: A wins ties, irrespective of value.
/// This works for memory versus SST reads and upper versus lower compaction inputs.
/// We store cursors; key()/value() borrow the winner and next() advances lazily.
/// No engine lock is acquired here, although child operations can perform I/O.
pub struct TwoMergeIterator<A: StorageIterator, B: StorageIterator> {
    a: A,
    b: B,
    // Cached selection for key(), value(), and is_valid(); not a recency flag.
    choose_a: bool,
}

impl<
    A: 'static + StorageIterator,
    B: 'static + for<'a> StorageIterator<KeyType<'a> = A::KeyType<'a>>,
> TwoMergeIterator<A, B>
{
    fn choose_a(a: &A, b: &B) -> bool {
        // Exhausted A selects B (even if B is also exhausted, making us invalid).
        // Otherwise, exhausted B selects A. Only compare keys when both are valid.
        if !a.is_valid() {
            return false;
        }
        if !b.is_valid() {
            return true;
        }
        // `skip_b` has already removed a tie, so a strict comparison is enough.
        a.key() < b.key()
    }

    fn skip_b(&mut self) -> Result<()> {
        // General duplicate rule: when both cursors have the same key, keep A's
        // entry and advance B past its copy. The caller gives A higher priority
        // by placing it first; this function does not inspect either value.
        // Example: A=[b:9,d:4], B=[b:2,c:3]. Advance B to c and expose A's b:9.
        // The full output is [b:9,c:3,d:4], with b emitted only once.
        // Each child already emits unique keys, so one step removes the duplicate.
        // A deletion follows exactly the same rule: if A's b holds a tombstone,
        // retain it here and discard B's b. The consumer decides its meaning:
        // read wrappers hide it; compaction may need to retain it in a new SST.
        // This helper itself neither removes tombstones nor selects visible versions.
        if self.a.is_valid() && self.b.is_valid() && self.b.key() == self.a.key() {
            self.b.next()?;
        }
        Ok(())
    }

    pub fn create(a: A, b: B) -> Result<Self> {
        // A and B may be different concrete types, but must expose the same key
        // type for comparison (the for<'a> bound above applies to every borrow).
        let mut iter = Self {
            choose_a: false,
            a,
            b,
        };
        // Normalize the initial position too; callers can read before calling next.
        iter.skip_b()?;
        iter.choose_a = Self::choose_a(&iter.a, &iter.b);
        Ok(iter)
    }
}

impl<
    A: 'static + StorageIterator,
    B: 'static + for<'a> StorageIterator<KeyType<'a> = A::KeyType<'a>>,
> StorageIterator for TwoMergeIterator<A, B>
{
    type KeyType<'a> = A::KeyType<'a>;

    fn key(&self) -> Self::KeyType<'_> {
        if self.choose_a {
            self.a.key()
        } else {
            self.b.key()
        }
    }

    fn value(&self) -> &[u8] {
        if self.choose_a {
            self.a.value()
        } else {
            self.b.value()
        }
    }

    fn is_valid(&self) -> bool {
        if self.choose_a {
            self.a.is_valid()
        } else {
            self.b.is_valid()
        }
    }

    fn next(&mut self) -> Result<()> {
        // Advance only the child that supplied the current output, then resolve
        // any new tie before exposing the next smallest key.
        // Continuing skip_b's example: A moves b->d; B stays at c, so B wins.
        // On the following call B moves past c; A's d becomes the next output.
        if self.choose_a {
            self.a.next()?;
        } else {
            self.b.next()?;
        }
        self.skip_b()?;
        self.choose_a = Self::choose_a(&self.a, &self.b);
        Ok(())
    }

    fn num_active_iterators(&self) -> usize {
        self.a.num_active_iterators() + self.b.num_active_iterators()
    }
}
