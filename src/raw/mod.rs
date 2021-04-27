#[cfg(any(test, miri))]
pub(crate) const R: usize = 4;
#[cfg(not(any(test, miri)))]
const R: usize = 8;

use core::iter::FusedIterator;
use core::mem;
use hashbrown::{raw, TryReserveError};

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
    pub const fn new() -> Self {
        Self {
            table: raw::RawTable::new(),
            leftovers: None,
        }
    }

    /// Attempts to allocate a new hash table with at least enough capacity
    /// for inserting the given number of elements without reallocating.
    #[cfg(feature = "raw")]
    pub fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        Ok(Self {
            table: raw::RawTable::try_with_capacity(capacity)?,
            leftovers: None,
        })
    }

    /// Allocates a new hash table with at least enough capacity for inserting
    /// the given number of elements without reallocating.
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

    /// Erases an element from the table, dropping it in place.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn erase(&mut self, item: Bucket<T>) {
        if item.in_main {
            self.table.erase(item.bucket);
        } else if let Some(ref mut lo) = self.leftovers {
            lo.items.reflect_remove(&item.bucket);
            lo.table.erase(item.bucket);
        } else {
            unreachable!("invalid bucket state");
        }
    }

    /// Finds and erases an element from the table, dropping it in place.
    /// Returns true if an element was found.
    #[cfg(feature = "raw")]
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn erase_entry(&mut self, hash: u64, eq: impl FnMut(&T) -> bool) -> bool {
        // Avoid `Option::map` because it bloats LLVM IR.
        if let Some(bucket) = self.find(hash, eq) {
            unsafe { self.erase(bucket) };
            true
        } else {
            false
        }
    }

    /// Removes an element from the table, returning it.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn remove(&mut self, item: Bucket<T>) -> T {
        if item.in_main {
            self.table.remove(item.bucket)
        } else if let Some(ref mut lo) = self.leftovers {
            lo.items.reflect_remove(&item.bucket);
            let v = lo.table.remove(item.bucket);

            if lo.table.len() == 0 {
                let _ = self.leftovers.take();
            }

            v
        } else {
            unreachable!("invalid bucket state");
        }
    }

    /// Finds and removes an element from the table, returning it.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn remove_entry(&mut self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<T> {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.find(hash, eq) {
            Some(bucket) => Some(unsafe { self.remove(bucket) }),
            None => None,
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
    ///
    /// While we try to make this incremental where possible, it may require all-at-once resizing.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64) {
        let need = self.leftovers.as_ref().map_or(0, |t| t.table.len()) + additional;
        if self.table.capacity() - self.table.len() > need {
            // We can accommodate the additional items without resizing, so all is well.
            if cfg!(debug_assertions) {
                let buckets = self.table.buckets();
                self.table.reserve(need, |_| unreachable!());
                assert_eq!(
                    buckets,
                    self.table.buckets(),
                    "resize despite sufficient capacity"
                );
            } else {
                self.table.reserve(need, |_| unreachable!());
            }
        } else if self.leftovers.is_some() {
            // We probably have to resize, but we already have leftovers!
            //
            // Here, we're sort of stuck — we can't do this fully incrementally, because we'd need
            // to keep _three_ tables: the current leftovers, the current table (which would become
            // the new leftovers), _and_ the new, resized table.
            //
            // We do the best we can, which is to carry over all the current leftovers, and _then_
            // do an incremental resize. This at least moves only the current leftovers, rather
            // than the current full set of elements.
            self.carry_all(hasher);
            self.grow(additional);
        } else {
            // We probably have to resize, but since we don't have any leftovers, we can do it
            // incrementally.
            self.grow(additional);
        }
    }

    /// Tries to ensure that at least `additional` items can be inserted into
    /// the table without reallocation.
    ///
    /// While we try to make this incremental where possible, it may require all-at-once resizing.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_reserve(
        &mut self,
        additional: usize,
        hasher: impl Fn(&T) -> u64,
    ) -> Result<(), TryReserveError> {
        let need = self.leftovers.as_ref().map_or(0, |t| t.table.len()) + additional;
        if self.table.capacity() - self.table.len() > need {
            // we can accommodate the additional items without resizing, so all good
            if cfg!(debug_assertions) {
                let buckets = self.table.buckets();
                self.table
                    .try_reserve(need, |_| unreachable!())
                    .expect("resize despite sufficient capacity");
                assert_eq!(
                    buckets,
                    self.table.buckets(),
                    "resize despite sufficient capacity"
                );
            } else {
                self.table
                    .try_reserve(need, |_| unreachable!())
                    .expect("resize despite sufficient capacity");
            }
            Ok(())
        } else if self.leftovers.is_some() {
            self.carry_all(hasher);
            self.try_grow(additional, true)
        } else {
            self.try_grow(additional, true)
        }
    }

    /// Inserts a new element into the table, and returns its raw bucket.
    ///
    /// This does not check if the given element already exists in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) -> Bucket<T> {
        if self.table.capacity() == self.table.len() {
            assert!(self.leftovers.is_none());
            // Even though this _may_ succeed without growing due to tombstones, handling
            // that case is convoluted, so we just assume this would grow the map.
            self.grow(1);
            return self.insert(hash, value, hasher);
        }

        self.insert_no_grow(hash, value, hasher)
    }

    /// Inserts a new element into the table, and returns a mutable reference to it.
    ///
    /// This does not check if the given element already exists in the table.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn insert_entry(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) -> &mut T {
        unsafe { self.insert(hash, value, hasher).as_mut() }
    }

    /// Inserts a new element into the table, without growing the table.
    ///
    /// There must be enough space in the table to insert the new element.
    ///
    /// This does not check if the given element already exists in the table.
    ///
    /// Note that unlike `hashbrown::RawTable::insert_no_grow`, this _does_ take a `hasher`.
    /// This is because while the insert won't grow the table, it may need to carry over some
    /// elements from the pre-resize table to the current table, which requires re-hashing.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn insert_no_grow(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) -> Bucket<T> {
        let bucket = self.table.insert_no_grow(hash, value);

        if self.leftovers.is_some() {
            // Also carry some items over.
            self.carry(hasher);
        }

        Bucket {
            bucket,
            in_main: true,
        }
    }

    /// Temporary removes a bucket, applying the given function to the removed
    /// element and optionally put back the returned value in the same bucket.
    ///
    /// Returns `true` if the bucket still contains an element
    ///
    /// This does not check if the given bucket is actually occupied.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn replace_bucket_with<F>(&mut self, bucket: Bucket<T>, f: F) -> bool
    where
        F: FnOnce(T) -> Option<T>,
    {
        if bucket.in_main {
            self.table.replace_bucket_with(bucket.bucket, f)
        } else if let Some(ref mut lo) = self.leftovers {
            let items = &mut lo.items;
            let b = bucket.bucket.clone();
            lo.table.replace_bucket_with(b, move |t| {
                let v = f(t);
                if v.is_none() {
                    items.reflect_remove(&bucket.bucket);
                }
                v
            })
        } else {
            unreachable!("invalid bucket state");
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

    /// Gets a reference to an element in the table.
    #[inline]
    pub fn get(&self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<&T> {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.find(hash, eq) {
            Some(bucket) => Some(unsafe { bucket.as_ref() }),
            None => None,
        }
    }

    /// Gets a mutable reference to an element in the table.
    #[inline]
    pub fn get_mut(&mut self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<&mut T> {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.find(hash, eq) {
            Some(bucket) => Some(unsafe { bucket.as_mut() }),
            None => None,
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

    /// Returns an iterator which removes all elements from the table without
    /// freeing the memory.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn drain(&mut self) -> RawDrain<'_, T> {
        RawDrain {
            table: self.table.drain(),
            leftovers: self
                .leftovers
                .take()
                .map(|lo| unsafe { lo.table.into_iter_from(lo.items) }),
        }
    }

    /// Returns an iterator which consumes all elements from the table.
    ///
    /// Iteration starts at the provided iterator's current location.
    ///
    /// It is up to the caller to ensure that the iterator is valid for this
    /// `RawTable` and covers all items that remain in the table.
    pub unsafe fn into_iter_from(self, iter: RawIter<T>) -> RawIntoIter<T> {
        RawIntoIter {
            table: self.table.into_iter_from(iter.table),
            leftovers: self.leftovers.map(|lo| lo.table.into_iter_from(lo.items)),
        }
    }
}

fn and_carry_with_hasher<T: Clone>(
    table: &mut raw::RawTable<T>,
    leftovers: &Option<OldTable<T>>,
    hasher: impl Fn(&T) -> u64,
) {
    if let Some(lo) = leftovers {
        for e in lo.items.clone() {
            let v = unsafe { e.as_ref() };
            let hash = hasher(v);
            table.insert(hash, v.clone(), &hasher);
        }
    }
}

impl<T: Clone> RawTable<T> {
    /// Variant of `clone_from` to use when a hasher is available.
    pub fn clone_from_with_hasher(&mut self, source: &Self, hasher: impl Fn(&T) -> u64) {
        let _ = self.leftovers.take();
        self.table.clone_from_with_hasher(&source.table, &hasher);
        // Since we're doing the work of cloning anyway, we might as well carry the leftovers.
        and_carry_with_hasher(&mut self.table, &source.leftovers, hasher);
    }

    /// Variant of `clone` to use when a hasher is available.
    pub fn clone_with_hasher(&self, hasher: impl Fn(&T) -> u64) -> Self {
        let mut table = self.table.clone();
        // Since we're doing the work of cloning anyway, we might as well carry the leftovers.
        and_carry_with_hasher(&mut table, &self.leftovers, hasher);
        RawTable {
            table,
            leftovers: None,
        }
    }
}

impl<T> RawTable<T> {
    #[cold]
    #[inline(never)]
    fn grow(&mut self, extra: usize) {
        if self.try_grow(extra, false).is_err() {
            unsafe { core::hint::unreachable_unchecked() };
        }
    }

    #[cold]
    fn try_grow(&mut self, extra: usize, fallible: bool) -> Result<(), TryReserveError> {
        debug_assert!(self.leftovers.is_none());

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
        // We also need to make sure we can fit the additional capacity required for `extra`.
        // Normally, that'll be handled by `inserts`, but not always!
        let add = usize::max(extra, inserts);
        let new_table = if fallible {
            raw::RawTable::try_with_capacity(need + inserts + add)?
        } else {
            raw::RawTable::with_capacity(need + inserts + add)
        };
        let old_table = mem::replace(&mut self.table, new_table);
        if old_table.len() != 0 {
            let old_table_items = unsafe { old_table.iter() };
            self.leftovers = Some(OldTable {
                table: old_table,
                items: old_table_items,
            });
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn carry_all(&mut self, hasher: impl Fn(&T) -> u64) {
        if let Some(ref mut lo) = self.leftovers {
            // It is safe to continue to access this iterator because:
            //  - we have not de-allocated the table it points into
            //  - we have not grown or shrunk the table it points into
            //
            // NOTE: Calling next here could be expensive, as the iter needs to search for the
            // next non-empty bucket. as the map grows in size, that search time will increase
            // linearly.
            while let Some(e) = lo.items.next() {
                // We need to remove the item in this bucket from the old map
                // to the resized map, without shrinking the old map.
                let value = unsafe { lo.table.remove(e) };
                let hash = hasher(&value);
                self.table.insert(hash, value, &hasher);
            }
            // The resize is finally fully complete.
            let _ = self.leftovers.take();
        }
    }

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
                    let value = unsafe { lo.table.remove(e) };
                    let hash = hasher(&value);
                    self.table.insert_no_grow(hash, value);
                } else {
                    // The resize is finally fully complete.
                    let _ = self.leftovers.take();
                    return;
                }
            }

            if lo.table.len() == 0 {
                // The resize is finally fully complete.
                let _ = self.leftovers.take();
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
        unsafe {
            let iter = self.iter();
            self.into_iter_from(iter)
        }
    }
}

struct OldTable<T> {
    table: raw::RawTable<T>,

    // We cache an iterator over the old table's buckets so we don't need to do a linear search
    // across buckets we know are empty each time we want to move more items.
    items: raw::RawIter<T>,
}

/// Iterator which returns a raw pointer to every full bucket in the table.
///
/// For maximum flexibility this iterator is not bound by a lifetime, but you
/// must observe several rules when using it:
/// - You must not free the hash table while iterating (including via growing/shrinking).
/// - It is fine to erase a bucket that has been yielded by the iterator.
/// - Erasing a bucket that has not yet been yielded by the iterator may still
///   result in the iterator yielding that bucket (unless `reflect_remove` is called).
/// - It is unspecified whether an element inserted after the iterator was
///   created will be yielded by that iterator (unless `reflect_insert` is called).
/// - The order in which the iterator yields bucket is unspecified and may
///   change in the future.
pub struct RawIter<T> {
    table: raw::RawIter<T>,
    leftovers: Option<raw::RawIter<T>>,
}

impl<T> RawIter<T> {
    /// Refresh the iterator so that it reflects a removal from the given bucket.
    ///
    /// For the iterator to remain valid, this method must be called once
    /// for each removed bucket before `next` is called again.
    ///
    /// This method should be called _before_ the removal is made. It is not necessary to call this
    /// method if you are removing an item that this iterator yielded in the past.
    #[cfg(feature = "raw")]
    pub fn reflect_remove(&mut self, b: &Bucket<T>) {
        if b.in_main {
            self.table.reflect_remove(b)
        } else if let Some(ref mut lo) = self.leftovers {
            // NOTE: The actuall call to erase/remove will take care of calling reflect_remove in
            // the "main" leftover's iterator.
            lo.reflect_remove(b)
        } else {
            unreachable!("invalid bucket state");
        }
    }

    /// Refresh the iterator so that it reflects an insertion into the given bucket.
    ///
    /// For the iterator to remain valid, this method must be called once
    /// for each insert before `next` is called again.
    ///
    /// This method does not guarantee that an insertion of a bucket witha greater
    /// index than the last one yielded will be reflected in the iterator.
    ///
    /// This method should be called _after_ the given insert is made.
    #[cfg(feature = "raw")]
    pub fn reflect_insert(&mut self, b: &Bucket<T>) {
        assert!(b.in_main, "no insertion can happen into leftovers");
        self.table.reflect_insert(b)
    }
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
        if let Some(ref mut lo) = self.leftovers {
            if let Some(e) = lo.next() {
                return Some(e);
            }
            // Done with leftovers.
            let _ = self.leftovers.take();
        }
        self.table.next()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter().size_hint()
    }
}

impl<T> ExactSizeIterator for RawIntoIter<T> {}
impl<T> FusedIterator for RawIntoIter<T> {}

/// Iterator which consumes elements without freeing the table storage.
pub struct RawDrain<'a, T> {
    table: raw::RawDrain<'a, T>,
    leftovers: Option<raw::RawIntoIter<T>>,
}

impl<T> RawDrain<'_, T> {
    /// Returns a by-reference iterator over the remaining items of this iterator.
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn iter(&self) -> RawIter<T> {
        RawIter {
            table: self.table.iter(),
            leftovers: self.leftovers.as_ref().map(|lo| lo.iter()),
        }
    }
}

impl<T> Drop for RawDrain<'_, T> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn drop(&mut self) {}
}

impl<T> Iterator for RawDrain<'_, T> {
    type Item = T;

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<T> {
        if let Some(ref mut lo) = self.leftovers {
            if let Some(e) = lo.next() {
                return Some(e);
            }
            // Done with leftovers.
            let _ = self.leftovers.take();
        }
        self.table.next()
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

impl<T> ExactSizeIterator for RawDrain<'_, T> {}
impl<T> FusedIterator for RawDrain<'_, T> {}
