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
//!
//! # Benchmarks
//!
//! There is a silly, but illustrative benchmark in `benches/vroom.rs`. It just runs lots of
//! inserts back-to-back, and measures how long each one takes. The problem quickly becomes
//! apparent:
//!
//! ```console
//! $ cargo bench --bench vroom > data.txt
//! hashbrown::HashMap max: 25.481194ms, mean: 1.349µs
//! griddle::HashMap max: 1.700794ms, mean: 1.362µs
//! ```
//!
//! You can see that the standard library implementation (through `hashbrown`) has some pretty
//! severe latency spikes. This is more readily visible through a timeline latency plot
//! (`misc/vroom.plt`):
//!
//! ![latency spikes on resize](misc/vroom.png)
//!
//! Resizes happen less frequently as the map grows, but they also take longer _when_ they occur.
//! With griddle, those spikes are mostly gone. There is a small linear component left, which I
//! believe comes from the work required to find the buckets that hold elements that must be moved.
//!
//! # Why griddle?
//!
//! You can amortize the cost of making hashbrowns by using a griddle..?
//!
//! [`hashbrown`]: https://crates.io/crates/hashbrown
//! [significant spikes]: https://twitter.com/jonhoo/status/1277618908355313670

#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

#[cfg(test)]
#[macro_use]
extern crate std;

mod map;
pub mod raw;

pub use map::HashMap;
