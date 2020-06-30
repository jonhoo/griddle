#[cfg(test)]
pub(crate) const R: usize = 4;
#[cfg(not(test))]
const R: usize = 8;

use core::iter::FusedIterator;
use core::mem;
use hashbrown::raw;

/// A reference to a hash table bucket containing a `T`.
///
/// This is usually just a pointer to the element itself. However if the element
/// is a ZST, then we instead track the index of the element in the table so
/// that `erase` works properly.
pub struct Bucket<T> {
    pub(crate) bucket: raw::Bucket<T>,
    pub(crate) in_main: bool,
}

impl<T> Clone for Bucket<T> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn clone(&self) -> Self {
        Bucket {
            bucket: self.bucket.clone(),
            in_main: self.in_main,
        }
    }
}

impl<T> Bucket<T> {
    /// Returns true if this bucket is in the "old" table and will be moved.
    pub fn will_move(&self) -> bool {
        !self.in_main
    }
}

impl<T> core::ops::Deref for Bucket<T> {
    type Target = raw::Bucket<T>;
    fn deref(&self) -> &Self::Target {
        &self.bucket
    }
}

/// A raw hash table with an unsafe API.
///
/// This is a wrapper around [`hashbrown::raw::RawTable`] that also implements incremental
/// resizing. When you interact with this API, keep in mind that there may be two backing tables,
/// and a lookup may return a reference to _either_. Eventually, entries in the old table will be
/// reclaimed, which invalidates any references to them.
#[derive(Clone)]
pub struct RawTable<T> {
    table: raw::RawTable<T>,
    leftovers: Option<OldTable<T>>,
}

impl<T> RawTable<T> {
    /// Creates a new empty hash table without allocating any memory.
    ///
    /// In effect this returns a table with exactly 1 bucket. However we can
    /// leave the data pointer dangling since that bucket is never written to
    /// due to our load factor forcing us to always have at least 1 free bucket.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn new() -> Self {
        Self {
            table: raw::RawTable::new(),
            leftovers: None,
        }
    }

    /// Allocates a new hash table with at least enough capacity for inserting
    /// the given number of elements without reallocating.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            table: raw::RawTable::with_capacity(capacity),
            leftovers: None,
        }
    }

    /// Returns a pointer to an element in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn bucket(&self, index: usize) -> Bucket<T> {
        Bucket {
            bucket: self.table.bucket(index),
            in_main: true,
        }
    }

    /// Erases an element from the table without dropping it.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn erase_no_drop(&mut self, item: &Bucket<T>) {
        if item.in_main {
            self.table.erase_no_drop(item);
        } else if let Some(ref mut lo) = self.leftovers {
            lo.table.erase_no_drop(item);

            if lo.table.len() == 0 {
                let _ = self.leftovers.take();
            } else {
                // By changing the state of the table, we have invalidated the table iterator
                // we keep for what elements are left to move. So, we re-compute it.
                //
                // TODO: We should be able to "fix up" the iterator rather than replace it,
                // which would save us from iterating over the prefix of empty buckets we've
                // left in our wake from the moves so far.
                lo.items = lo.table.iter();
            }
        } else {
            unreachable!("invalid bucket state");
        }
    }

    /// Marks all table buckets as empty without dropping their contents.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn clear_no_drop(&mut self) {
        self.table.clear_no_drop();
        if let Some(mut lo) = self.leftovers.take() {
            lo.table.clear_no_drop();
        }
    }

    /// Removes all elements from the table without freeing the backing memory.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn clear(&mut self) {
        let _ = self.leftovers.take();
        self.table.clear();
    }

    /// Shrinks the table so that it fits as close to `min_size` elements as possible.
    ///
    /// In reality, the table may end up larger than `min_size`, as must be able to hold all the
    /// current elements, as well as some additional elements due to incremental resizing.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn shrink_to(&mut self, min_size: usize, hasher: impl Fn(&T) -> u64) {
        // Calculate the minimal number of elements that we need to reserve
        // space for.
        let mut need = self.table.len();
        // We need to make sure that we never have to resize while there
        // are still leftovers.
        if let Some(ref lo) = self.leftovers {
            // We need to move another lo.table.len() items.
            need += lo.table.len();
            // We move R items on each insert.
            // That means we need to accomodate another
            // lo.table.len() / R (rounded up) inserts to move them all.
            need += (lo.table.len() + R - 1) / R;
        }
        let min_size = usize::max(need, min_size);
        self.table.shrink_to(min_size, hasher);
    }

    /// Ensures that at least `additional` items can be inserted into the table
    /// without reallocation.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64) {
        self.table.reserve(
            self.leftovers.as_ref().map_or(0, |t| t.table.len()) + additional,
            hasher,
        )
    }

    /// Tries to ensure that at least `additional` items can be inserted into
    /// the table without reallocation.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_reserve(
        &mut self,
        additional: usize,
        hasher: impl Fn(&T) -> u64,
    ) -> Result<(), hashbrown::TryReserveError> {
        self.table.try_reserve(
            self.leftovers.as_ref().map_or(0, |t| t.table.len()) + additional,
            hasher,
        )
    }

    /// Inserts a new element into the table.
    ///
    /// This does not check if the given element already exists in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) -> Bucket<T> {
        let bucket = if self.leftovers.is_some() {
            let bucket = if cfg!(debug_assertions) {
                let cap = self.table.capacity();
                let b = self.table.insert(hash, value, &hasher);

                // make sure table didn't resize
                // the +1 is because the insertion may have discovered that it was able to re-use a
                // tombstone, in which it realizes its capacity is actually one greater.
                assert!(
                    cap <= self.table.capacity() + 1,
                    "resize while elements are still left over"
                );
                b
            } else {
                self.table.insert(hash, value, &hasher)
            };
            // Also carry some items over.
            self.carry(hasher);
            bucket
        } else if self.table.capacity() == self.table.len() {
            // Even though this _may_ succeed without growing due to tombstones, handling
            // that case is convoluted, so we just assume this would grow the map.
            //
            // We need to grow the table by at least a factor of (R + 1)/R to ensure that
            // the new table won't _also_ grow while we're still moving items from the old
            // one.
            //
            // Here's how we get to len * (R + 1)/R:
            //  - We need to move another len items
            let need = self.table.len();
            //  - We move R items on each insert, so to move len items takes
            //    len / R inserts (rounded up!)
            //  - Since we want to round up, we pull the old +R-1 trick
            let inserts = (self.table.len() + R - 1) / R;
            //  - That's len + len/R
            //    Which is == R*len/R + len/R
            //    Which is == ((R+1)*len)/R
            //    Which is == len * (R+1)/R
            //  - We don't actually use that formula because of integer division.
            //
            // We don't normally need to do +1 here to account for the ongoing insert, since
            // that'll be wrapped up in `inserts`. But we do anyway to handle the case where
            // the table is _currently_ empty.
            let mut new_table = raw::RawTable::with_capacity(need + inserts + 1);

            let bucket = new_table.insert(hash, value, &hasher);
            let old_table = mem::replace(&mut self.table, new_table);
            let old_table_items = unsafe { old_table.iter() };
            self.leftovers = Some(OldTable {
                table: old_table,
                items: old_table_items,
            });
            self.carry(hasher);
            bucket
        } else {
            self.table.insert(hash, value, hasher)
        };
        Bucket {
            bucket,
            in_main: true,
        }
    }

    /// Searches for an element in the table.
    #[inline]
    pub fn find(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<Bucket<T>> {
        let e = self.table.find(hash, &mut eq);
        if let Some(bucket) = e {
            return Some(Bucket {
                bucket,
                in_main: true,
            });
        }

        if let Some(OldTable { ref table, .. }) = self.leftovers {
            table.find(hash, eq).map(|bucket| Bucket {
                bucket,
                in_main: false,
            })
        } else {
            None
        }
    }

    /// Returns the number of elements the map can hold without reallocating.
    ///
    /// This number is a lower bound; the table might be able to hold
    /// more, but is guaranteed to be able to hold at least this many.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Returns the number of elements in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn len(&self) -> usize {
        self.table.len() + self.leftovers.as_ref().map_or(0, |t| t.table.len())
    }

    /// Returns the number of buckets in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn buckets(&self) -> usize {
        self.table.buckets()
    }

    /// Returns an iterator over every element in the table. It is up to
    /// the caller to ensure that the `RawTable` outlives the `RawIter`.
    /// Because we cannot make the `next` method unsafe on the `RawIter`
    /// struct, we have to make the `iter` method unsafe.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn iter(&self) -> RawIter<T> {
        RawIter {
            table: self.table.iter(),
            leftovers: self.leftovers.as_ref().map(|lo| lo.items.clone()),
        }
    }
}

impl<T: Clone> RawTable<T> {
    /// Variant of `clone_from` to use when a hasher is available.
    #[cfg(feature = "raw")]
    pub fn clone_from_with_hasher(&mut self, source: &Self, hasher: impl Fn(&T) -> u64) {
        self.table.clone_from_with_hasher(&source.table, &hasher);
        if let Some(ref lo_) = source.leftovers {
            if let Some(ref mut lo) = self.leftovers {
                lo.table.clone_from_with_hasher(&lo_.table, hasher);
                lo.items = unsafe { lo.table.iter() };
            } else {
                self.leftovers = Some(lo_.clone());
            }
        }
    }
}

impl<T> RawTable<T> {
    #[cold]
    #[inline(never)]
    pub(crate) fn carry(&mut self, hasher: impl Fn(&T) -> u64) {
        if let Some(ref mut lo) = self.leftovers {
            for _ in 0..R {
                // It is safe to continue to access this iterator because:
                //  - we have not de-allocated the table it points into
                //  - we have not grown or shrunk the table it points into
                //
                // NOTE: Calling next here could be expensive, as the iter needs to search for the
                // next non-empty bucket. as the map grows in size, that search time will increase
                // linearly.
                if let Some(e) = lo.items.next() {
                    // We need to remove the item in this bucket from the old map
                    // to the resized map, without shrinking the old map.
                    let value = unsafe {
                        let v = e.read();
                        lo.table.erase_no_drop(&e);
                        v
                    };
                    let hash = hasher(&value);
                    self.table.insert(hash, value, &hasher);
                } else {
                    // The resize is finally fully complete.
                    let _ = self.leftovers.take();
                    break;
                }
            }
        }
    }

    pub(crate) fn is_split(&self) -> bool {
        self.leftovers.is_some()
    }

    #[cfg(any(test, feature = "rayon"))]
    pub(crate) fn main(&self) -> &raw::RawTable<T> {
        &self.table
    }

    #[cfg(any(test, feature = "rayon"))]
    pub(crate) fn leftovers(&self) -> Option<&raw::RawTable<T>> {
        self.leftovers.as_ref().map(|lo| &lo.table)
    }
}

impl<T> IntoIterator for RawTable<T> {
    type Item = T;
    type IntoIter = RawIntoIter<T>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn into_iter(self) -> RawIntoIter<T> {
        RawIntoIter {
            table: self.table.into_iter(),
            leftovers: self.leftovers.map(|lo| {
                // TODO: make this re-use knowledge of progress from lo.items
                lo.table.into_iter()
            }),
        }
    }
}

struct OldTable<T> {
    table: raw::RawTable<T>,

    // We cache an iterator over the old table's buckets so we don't need to do a linear search
    // across buckets we know are empty each time we want to move more items.
    items: raw::RawIter<T>,
}

impl<T: Clone> Clone for OldTable<T> {
    fn clone(&self) -> OldTable<T> {
        let table = self.table.clone();
        let items = unsafe { table.iter() };
        OldTable { table, items }
    }

    fn clone_from(&mut self, source: &Self) {
        self.table.clone_from(&source.table);
        self.items = unsafe { self.table.iter() };
    }
}

/// Iterator which returns a raw pointer to every full bucket in the table.
pub struct RawIter<T> {
    table: raw::RawIter<T>,
    leftovers: Option<raw::RawIter<T>>,
}

impl<T> Clone for RawIter<T> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
            leftovers: self.leftovers.clone(),
        }
    }
}

impl<T> Iterator for RawIter<T> {
    type Item = Bucket<T>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<Self::Item> {
        let leftovers = &mut self.leftovers;
        self.table
            .next()
            .map(|bucket| Bucket {
                bucket,
                in_main: true,
            })
            .or_else(|| {
                leftovers.as_mut()?.next().map(|bucket| Bucket {
                    bucket,
                    in_main: false,
                })
            })
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (mut lo, mut hi) = self.table.size_hint();
        if let Some(ref left) = self.leftovers {
            let (lo2, hi2) = left.size_hint();
            lo += lo2;
            if let (Some(ref mut hi), Some(hi2)) = (&mut hi, hi2) {
                *hi += hi2;
            }
        }
        (lo, hi)
    }
}

impl<T> ExactSizeIterator for RawIter<T> {}
impl<T> FusedIterator for RawIter<T> {}

/// Iterator which consumes a table and returns elements.
pub struct RawIntoIter<T> {
    table: raw::RawIntoIter<T>,
    leftovers: Option<raw::RawIntoIter<T>>,
}

impl<T> RawIntoIter<T> {
    /// Returns a by-reference iterator over the remaining items of this iterator.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn iter(&self) -> RawIter<T> {
        RawIter {
            table: self.table.iter(),
            leftovers: self.leftovers.as_ref().map(|lo| lo.iter()),
        }
    }
}

impl<T> Iterator for RawIntoIter<T> {
    type Item = T;

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<T> {
        let leftovers = &mut self.leftovers;
        self.table.next().or_else(|| leftovers.as_mut()?.next())
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter().size_hint()
    }
}

impl<T> ExactSizeIterator for RawIntoIter<T> {}
impl<T> FusedIterator for RawIntoIter<T> {}
