use crate::raw::Bucket;
use crate::raw::RawTable;
use rayon_::iter::{
    plumbing::{Reducer, UnindexedConsumer},
    ParallelIterator,
};

/// Parallel iterator which returns a raw pointer to every full bucket in the table.
pub struct RawParIter<T> {
    iter: hashbrown::raw::rayon::RawParIter<T>,
    leftovers: Option<hashbrown::raw::rayon::RawParIter<T>>,
}

impl<T> ParallelIterator for RawParIter<T> {
    type Item = Bucket<T>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        if let Some(lo) = self.leftovers {
            let left = consumer.split_off_left();
            let reducer = consumer.to_reducer();
            let left_half = self
                .iter
                .map(|bucket| Bucket {
                    bucket,
                    in_main: true,
                })
                .drive_unindexed(left);
            let right_half = lo
                .map(|bucket| Bucket {
                    bucket,
                    in_main: false,
                })
                .drive_unindexed(consumer);
            reducer.reduce(left_half, right_half)
        } else {
            self.iter
                .map(|bucket| Bucket {
                    bucket,
                    in_main: true,
                })
                .drive_unindexed(consumer)
        }
    }
}

impl<T> RawTable<T> {
    /// Returns a parallel iterator over the elements in a `RawTable`.
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn par_iter(&self) -> RawParIter<T> {
        RawParIter {
            iter: self.main().par_iter(),
            leftovers: self.leftovers().map(|t| t.iter().into()),
        }
    }
}
