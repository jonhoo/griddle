#![cfg(not(miri))]

#[macro_use]
extern crate quickcheck;

use griddle::HashMap as GriddleMap;

use quickcheck::Arbitrary;
use quickcheck::Gen;

use fnv::FnvHasher;
use std::hash::{BuildHasher, BuildHasherDefault};
type FnvBuilder = BuildHasherDefault<FnvHasher>;
type GriddleMapFnv<K, V> = GriddleMap<K, V, FnvBuilder>;

use std::cmp::min;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::iter::FromIterator;
use std::ops::Deref;

use griddle::hash_map::Entry as OEntry;
use std::collections::hash_map::Entry as HEntry;

fn set<'a, T: 'a, I>(iter: I) -> HashSet<T>
where
    I: IntoIterator<Item = &'a T>,
    T: Copy + Hash + Eq,
{
    iter.into_iter().cloned().collect()
}

quickcheck! {
    fn contains(insert: Vec<u32>) -> bool {
        let mut map = GriddleMap::new();
        for &key in &insert {
            map.insert(key, ());
        }
        insert.iter().all(|&key| map.get(&key).is_some())
    }

    fn contains_not(insert: Vec<u8>, not: Vec<u8>) -> bool {
        let mut map = GriddleMap::new();
        for &key in &insert {
            map.insert(key, ());
        }
        let nots = &set(&not) - &set(&insert);
        nots.iter().all(|&key| map.get(&key).is_none())
    }

    fn insert_remove(insert: Vec<u8>, remove: Vec<u8>) -> bool {
        let mut map = GriddleMap::new();
        for &key in &insert {
            map.insert(key, ());
        }
        for &key in &remove {
            map.remove(&key);
        }
        let elements = &set(&insert) - &set(&remove);
        map.len() == elements.len() && map.iter().count() == elements.len() &&
            elements.iter().all(|k| map.get(k).is_some())
    }

    fn insert_retain(insert: Vec<u8>, retain: Vec<u8>) -> bool {
        let mut map = GriddleMap::new();
        for &key in &insert {
            map.insert(key, ());
        }
        map.retain(|key, _| retain.contains(key));
        let insert = set(&insert);
        let retain = set(&retain);
        let elements: Vec<_> = insert.intersection(&retain).into_iter().collect();
        map.len() == elements.len() && map.iter().count() == elements.len() &&
            elements.iter().all(|k| map.get(k).is_some())
    }

    fn with_cap(cap: u16) -> bool {
        let map: GriddleMap<u8, u8> = GriddleMap::with_capacity(cap as usize);
        println!("wish: {}, got: {} (diff: {})", cap, map.capacity(), map.capacity() as isize - cap as isize);
        map.capacity() >= cap as usize
    }
}

use Op::*;
#[derive(Copy, Clone, Debug)]
enum Op<K, V> {
    Add(K, V),
    Remove(K),
    AddEntry(K, V),
    RemoveEntry(K),
    ShrinkToFit,
    ReplaceWithClone,
    Reserve(u16),
}

impl<K, V> Arbitrary for Op<K, V>
where
    K: Arbitrary,
    V: Arbitrary,
{
    fn arbitrary(g: &mut Gen) -> Self {
        match u32::arbitrary(g) % 6 {
            0 => Add(K::arbitrary(g), V::arbitrary(g)),
            1 => AddEntry(K::arbitrary(g), V::arbitrary(g)),
            2 => Remove(K::arbitrary(g)),
            3 => RemoveEntry(K::arbitrary(g)),
            4 => ShrinkToFit,
            5 => ReplaceWithClone,
            _ => Reserve(u16::arbitrary(g)),
        }
    }
}

fn do_ops<K, V, S>(ops: &[Op<K, V>], a: &mut GriddleMap<K, V, S>, b: &mut HashMap<K, V>)
where
    K: Hash + Eq + Clone,
    V: Clone,
    S: BuildHasher + Clone,
{
    for op in ops {
        match *op {
            Add(ref k, ref v) => {
                a.insert(k.clone(), v.clone());
                b.insert(k.clone(), v.clone());
            }
            AddEntry(ref k, ref v) => {
                a.entry(k.clone()).or_insert_with(|| v.clone());
                b.entry(k.clone()).or_insert_with(|| v.clone());
            }
            Remove(ref k) => {
                a.remove(k);
                b.remove(k);
            }
            RemoveEntry(ref k) => {
                if let OEntry::Occupied(ent) = a.entry(k.clone()) {
                    ent.remove_entry();
                }
                if let HEntry::Occupied(ent) = b.entry(k.clone()) {
                    ent.remove_entry();
                }
            }
            ShrinkToFit => {
                a.shrink_to_fit();
                b.shrink_to_fit();
            }
            ReplaceWithClone => {
                *a = a.clone();
                *b = b.clone();
            }
            Reserve(n) => {
                a.reserve(n as usize);
                b.reserve(n as usize);
            }
        }
        //println!("{:?}", a);
    }
}

fn assert_maps_equivalent<K, V>(a: &GriddleMap<K, V>, b: &HashMap<K, V>) -> bool
where
    K: Hash + Eq + Debug,
    V: Eq + Debug,
{
    assert_eq!(a.len(), b.len());
    assert_eq!(a.iter().next().is_some(), b.iter().next().is_some());
    for key in a.keys() {
        assert!(b.contains_key(key), "b does not contain {:?}", key);
    }
    for key in b.keys() {
        assert!(a.get(key).is_some(), "a does not contain {:?}", key);
    }
    for key in a.keys() {
        assert_eq!(a[key], b[key]);
    }
    true
}

quickcheck! {
    fn operations_i8(ops: Large<Vec<Op<i8, i8>>>) -> bool {
        let mut map = GriddleMap::new();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);
        assert_maps_equivalent(&map, &reference)
    }

    fn operations_string(ops: Vec<Op<Alpha, i8>>) -> bool {
        let mut map = GriddleMap::new();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);
        assert_maps_equivalent(&map, &reference)
    }

    fn keys_values(ops: Large<Vec<Op<i8, i8>>>) -> bool {
        let mut map = GriddleMap::new();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);
        let mut visit = GriddleMap::new();
        for (k, v) in map.keys().zip(map.values()) {
            assert_eq!(&map[k], v);
            assert!(!visit.contains_key(k));
            visit.insert(*k, *v);
        }
        assert_eq!(visit.len(), reference.len());
        true
    }

    fn keys_values_mut(ops: Large<Vec<Op<i8, i8>>>) -> bool {
        let mut map = GriddleMap::new();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);
        let mut visit = GriddleMap::new();
        let keys = Vec::from_iter(map.keys().cloned());
        for (k, v) in keys.iter().zip(map.values_mut()) {
            assert_eq!(&reference[k], v);
            assert!(!visit.contains_key(k));
            visit.insert(*k, *v);
        }
        assert_eq!(visit.len(), reference.len());
        true
    }

    fn equality(ops: Vec<Op<i8, i8>>) -> bool {
        let mut map = GriddleMap::new();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);

        assert_eq!(map.len(), reference.len());
        for (k, v) in map.iter() {
            assert_eq!(reference.get(k), Some(v), "k = {}", k);
        }
        for (k, v) in reference.iter() {
            assert_eq!(map.get(k), Some(v), "k = {}", k);
        }
        true
    }

    fn equality_fnv(ops: Vec<Op<i8, i8>>) -> bool {
        let mut map = GriddleMapFnv::default();
        let mut reference = HashMap::new();
        do_ops(&ops, &mut map, &mut reference);

        assert_eq!(map.len(), reference.len());
        for (k, v) in map.iter() {
            assert_eq!(reference.get(k), Some(v), "k = {}", k);
        }
        for (k, v) in reference.iter() {
            assert_eq!(map.get(k), Some(v), "k = {}", k);
        }
        true
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Alpha(String);

impl Deref for Alpha {
    type Target = String;
    fn deref(&self) -> &String {
        &self.0
    }
}

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

impl Arbitrary for Alpha {
    fn arbitrary(g: &mut Gen) -> Self {
        let len = u32::arbitrary(g) % g.size() as u32;
        let len = min(len, 16);
        Alpha(
            (0..len)
                .map(|_| g.choose(ALPHABET).copied().unwrap() as char)
                .collect(),
        )
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        Box::new((**self).shrink().map(Alpha))
    }
}

/// quickcheck Arbitrary adaptor -- make a larger vec
#[derive(Clone, Debug)]
struct Large<T>(T);

impl<T> Deref for Large<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> Arbitrary for Large<Vec<T>>
where
    T: Arbitrary,
{
    fn arbitrary(g: &mut Gen) -> Self {
        let len = u32::arbitrary(g) % (g.size() * 10) as u32;
        Large((0..len).map(|_| T::arbitrary(g)).collect())
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        Box::new((**self).shrink().map(Large))
    }
}
