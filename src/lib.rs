//! A `HashMap` variant that spreads resize load across inserts.
//!
//! Most hash table implementations (including [`hashbrown`], the one in Rust's standard library)
//! must occasionally "resize" the backing memory for the map as the number of elements grows.
//! This means allocating a new table (usually of twice the size), and moving all the elements from
//! the old table to the new one. As your table gets larger, this process takes longer and longer.
//!
//! For most applications, this behavior is fine — if some very small number of inserts take longer
//! than others, the application won't even notice. And if the map is relatively small anyway, even
//! those "slow" inserts are quite fast. Similarly, if your map grow for a while, and then _stops_
//! growing, the "steady state" of your application won't see any resizing pauses at all.
//!
//! Where resizing becomes a problem is in applications that use maps to keep ever-growing state
//! where tail latency is important. At large scale, it is simply not okay for one map insert to
//! take 30 milliseconds when most take single-digit **micro**seconds. Worse yet, these resize
//! pauses can compound to create [significant spikes] in tail latency.
//!
//! This crate implements a technique referred to as "incremental resizing", in contrast to the
//! common "all-at-once" approached outlined above. At its core, the idea is pretty simple: instead
//! of moving all the elements to the resized map immediately, move a couple each time an insert
//! happens. This spreads the cost of moving the elements so that _each_ insert becomes a little
//! slower until the resize has finished, instead of _one_ insert becoming a _lot_ slower.
//!
//! This approach isn't free, however. While the resize is going on, the old table must be
//! kept around (so memory isn't reclaimed immediately), and all reads must check both the old and
//! new map, which makes them slower. Only once the resize completes is the old table reclaimed and
//! full read performance restored.
//!
//! To help you decide whether this implementation is right for you, here's a handy reference for
//! how this implementation compares to the standard library map:
//!
//!  - Inserts all take approximately the same time.
//!    After a resize, they will be slower for a while, but only by a relatively small factor.
//!  - Memory is not reclaimed immediately upon resize.
//!  - Reads and removals of **old** or **missing** keys are slower for a while after a resize.
//!  - The incremental map is slightly larger on the stack.
//!  - The "efficiency" of the resize is slightly lower as the all-at-once resize moves the items
//!    from the small table to the large one in batch, whereas the incremental does a series of
//!    inserts.
//!
//! Under the hood, griddle uses `hashbrown::raw` to avoid re-implementing the core pieces of
//! `hashbrown`. It also stays _very_ close to `hashbrown`'s `HashMap` and `HashSet` wrappers, and
//! even exposes a [`raw`] module so that you can build your own map on top of griddle's modified
//! table behavior.
//!
//! # Why "griddle"?
//!
//! You can amortize the cost of making hashbrowns by using a griddle..?
//!
//! [`hashbrown`]: https://crates.io/crates/hashbrown
//! [significant spikes]: https://twitter.com/jonhoo/status/1277618908355313670

#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
// this is #[cfg(test)] so that Rust 1.42 can still compile the crate.
// tests don't have to commit to MSRV though, so 1.52 is fine there.
#![cfg_attr(test, warn(rustdoc::all))]
// hashbrown does this to avoid LLVM IR bloat in a few places.
#![allow(clippy::manual_map)]

#[cfg(test)]
#[macro_use]
extern crate std;

#[cfg_attr(test, macro_use)]
extern crate alloc;

#[cfg(feature = "raw")]
/// Experimental and unsafe `RawTable` API. This module is only available if the
/// `raw` feature is enabled.
pub mod raw {
    #[path = "mod.rs"]
    mod inner;
    pub use inner::*;

    #[cfg(feature = "rayon")]
    /// [rayon]-based parallel iterator types for raw hash tables.
    /// You will rarely need to interact with it directly unless you have need
    /// to name one of the iterator types.
    ///
    /// [rayon]: https://docs.rs/rayon/1.0/rayon
    pub mod rayon {
        pub use crate::external_trait_impls::rayon::raw::*;
    }
}
#[cfg(not(feature = "raw"))]
#[allow(dead_code)]
mod raw;

mod external_trait_impls;
mod map;
mod set;

pub mod hash_map {
    //! A hash map implemented with quadratic probing and SIMD lookup.
    pub use crate::map::*;

    #[cfg(feature = "rayon")]
    /// [rayon]-based parallel iterator types for hash maps.
    /// You will rarely need to interact with it directly unless you have need
    /// to name one of the iterator types.
    ///
    /// [rayon]: https://docs.rs/rayon/1.0/rayon
    pub mod rayon {
        pub use crate::external_trait_impls::rayon::map::*;
    }
}
pub mod hash_set {
    //! A hash set implemented as a `HashMap` where the value is `()`.
    pub use crate::set::*;

    #[cfg(feature = "rayon")]
    /// [rayon]-based parallel iterator types for hash sets.
    /// You will rarely need to interact with it directly unless you have need
    /// to name one of the iterator types.
    ///
    /// [rayon]: https://docs.rs/rayon/1.0/rayon
    pub mod rayon {
        pub use crate::external_trait_impls::rayon::set::*;
    }
}

pub use crate::map::HashMap;
pub use crate::set::HashSet;

pub use hashbrown::TryReserveError;
