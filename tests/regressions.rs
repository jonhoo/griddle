use griddle::hash_map::Entry;
use griddle::HashMap;

#[test]
fn reserve_shrink_add() {
    // [Reserve(18303), ShrinkToFit, Add(-94, -96)]
    let mut map = HashMap::new();
    map.reserve(18303);
    map.shrink_to_fit();
    map.insert(-94i8, -96i8);
}

#[test]
fn carry_moves_exactly() {
    // [AddEntry(-14, 67), Add(6, -14), AddEntry(29, 67), AddEntry(10, 82), Add(-33, -44), Add(37, 88), AddEntry(72, 73), Add(-90, 74), Reserve(45114), AddEntry(-75, -31), Remove(29), RemoveEntry(72), ShrinkToFit, AddEntry(42, -34)]
    let mut map = HashMap::new();
    map.entry(-14i8).or_insert(67);
    map.insert(6, -14);
    map.entry(29).or_insert(67);
    map.entry(10).or_insert(82);
    map.insert(-33, -44);
    map.insert(37, 88);
    map.entry(72).or_insert(73);
    map.entry(-90).or_insert(74);
    map.reserve(45114);
    map.entry(-75).or_insert(-31);
    map.remove(&29);
    if let Entry::Occupied(e) = map.entry(72) {
        e.remove_entry();
    }
    map.shrink_to_fit();
    map.entry(42).or_insert(-34);
}
