#[cfg(test)]
pub(crate) const R: usize = 4;
#[cfg(not(test))]
const R: usize = 8;

use core::iter::FusedIterator;
use core::mem;
use hashbrown::raw;

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

    /// Erases an element from the table without dropping it.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn erase_no_drop(&mut self, item: &Bucket<T>) {
        if item.in_main {
            self.table.erase_no_drop(item);
        } else if let Some(ref mut lo) = self.leftovers {
            lo.table.erase_no_drop(item);

            // By changing the state of the table, we have invalidated the table iterator
            // we keep for what elements are left to move. So, we re-compute it.
            //
            // TODO: We should be able to "fix up" the iterator rather than replace it,
            // which would save us from iterating over the prefix of empty buckets we've
            // left in our wake from the moves so far.
            lo.items = lo.table.iter();
        } else {
            unreachable!("invalid bucket state");
        }
    }

    /// Removes all elements from the table without freeing the backing memory.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn clear(&mut self) {
        let _ = self.leftovers.take();
        self.table.clear();
    }

    /// Inserts a new element into the table.
    ///
    /// This does not check if the given element already exists in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) -> Bucket<T> {
        let bucket = if self.leftovers.is_some() {
            let bucket = self.table.insert(hash, value, &hasher);
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
            let need_cap = ((R + 1) / R) * (self.table.len() + 1);
            let mut new_table = raw::RawTable::with_capacity(need_cap);

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
                        lo.table.erase_no_drop(&e);
                        e.read()
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

    #[cfg(test)]
    pub(crate) fn main(&self) -> &raw::RawTable<T> {
        &self.table
    }

    #[cfg(test)]
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

#[derive(Clone)]
struct OldTable<T> {
    table: raw::RawTable<T>,

    // We cache an iterator over the old table's buckets so we don't need to do a linear search
    // across buckets we know are empty each time we want to move more items.
    items: raw::RawIter<T>,
}

/// A reference to a hash table bucket containing a `T`.
///
/// This is usually just a pointer to the element itself. However if the element
/// is a ZST, then we instead track the index of the element in the table so
/// that `erase` works properly.
pub struct Bucket<T> {
    bucket: raw::Bucket<T>,
    in_main: bool,
}

impl<T> Bucket<T> {
    pub fn in_leftovers(&self) -> bool {
        !self.in_main
    }
}

impl<T> core::ops::Deref for Bucket<T> {
    type Target = raw::Bucket<T>;
    fn deref(&self) -> &Self::Target {
        &self.bucket
    }
}

/// Iterator which returns a raw pointer to every full bucket in the table.
///
/// Note that the buckets that are returned _may_ be in the old table that will soon be reclaimed.
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
