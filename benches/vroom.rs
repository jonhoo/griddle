use griddle::HashMap as IncrHashMap;
use hashbrown::HashMap;
use std::time::{Duration, Instant};

const N: u32 = 1 << 21;

fn main() {
    let mut hm = HashMap::new();
    let mut t = Instant::now();
    let mut mx = 0.0f64;
    let mut sum = Duration::new(0, 0);
    for i in 0..N {
        hm.insert(i, i);
        let t2 = Instant::now();
        let took = t2.duration_since(t);
        t = t2;
        mx = mx.max(took.as_secs_f64());
        sum += took;
        println!("{} hashbrown {} ms", i, took.as_secs_f64() * 1000.0);
    }
    eprintln!(
        "hashbrown::HashMap max: {:?}, mean: {:?}",
        Duration::from_secs_f64(mx),
        sum / N
    );

    let mut hm = IncrHashMap::new();
    let mut t = Instant::now();
    let mut mx = 0.0f64;
    let mut sum = Duration::new(0, 0);
    for i in 0..N {
        hm.insert(i, i);
        let t2 = Instant::now();
        let took = t2.duration_since(t);
        t = t2;
        mx = mx.max(took.as_secs_f64());
        sum += took;
        println!("{} griddle {} ms", i, took.as_secs_f64() * 1000.0);
    }
    eprintln!(
        "griddle::HashMap max: {:?}, mean: {:?}",
        Duration::from_secs_f64(mx),
        sum / N
    );
}
