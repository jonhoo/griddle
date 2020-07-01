use griddle::HashMap;

#[test]
fn reserve_shrink_add() {
    let mut map = HashMap::new();
    map.reserve(18303);
    map.shrink_to_fit();
    map.insert(-94i8, -96i8);
}
