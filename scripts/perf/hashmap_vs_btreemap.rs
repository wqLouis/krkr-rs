//! HashMap (SipHash) vs HashMap (FxHash) vs BTreeMap vs sorted Vec, by size and key kind.
//!
//! Answers a concrete question: for the small maps in this tree (registry and cache lookups
//! keyed by ids and short storage names), does the hasher cost more than an ordered structure?
//!
//! Build:  rustc -O /tmp/hashmap_vs_btreemap.rs -o /tmp/hashmap_vs_btreemap && /tmp/hashmap_vs_btreemap
//! No dependencies: the FxHash below is the standard rustc-hash algorithm, written out so the
//! comparison needs no crate.

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::hint::black_box;
use std::time::Instant;

/// rustc-hash's FxHasher: multiply-xor, no crypto.
#[derive(Default)]
struct FxHasher {
    hash: u64,
}

impl FxHasher {
    const K: u64 = 0x517c_c1b7_2722_0a95;
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(buf));
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.hash = (self.hash.rotate_left(5) ^ n).wrapping_mul(Self::K);
    }
    fn finish(&self) -> u64 {
        self.hash
    }
}

type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

fn keys_u64(n: usize) -> Vec<u64> {
    // Ids: dense-ish, as scene/bitmap/layer ids are.
    (0..n as u64).map(|i| i.wrapping_mul(2_654_435_761)).collect()
}

fn keys_name(n: usize) -> Vec<String> {
    // Short storage names, as `FRM_0130.webp`, `system/Initialize.tjs` are.
    (0..n).map(|i| format!("parts/char/FRM_{i:04}.webp")).collect()
}

fn bench<F: FnMut()>(mut f: F, iters: usize) -> f64 {
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_secs_f64() * 1e9 / iters as f64
}

fn report(label: &str, n: usize, build: f64, hit: f64, miss: f64, iter: f64) {
    println!(
        "{label:14} n={n:<6} build={build:>9.0}ns  lookup-hit={hit:>7.0}ns  lookup-miss={miss:>7.0}ns  iterate={iter:>9.0}ns"
    );
}

fn run_u64(n: usize) {
    let ks = keys_u64(n);
    let iters = 200;

    let mut m: HashMap<u64, u64> = HashMap::new();
    for (i, k) in ks.iter().enumerate() {
        m.insert(*k, i as u64);
    }
    let build = bench(|| {
        let mut x: HashMap<u64, u64> = HashMap::new();
        for (i, k) in ks.iter().enumerate() {
            x.insert(*k, i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *m.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss_key = u64::MAX;
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *m.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(m.values().sum::<u64>());
    }, iters);
    report("HashMap/Sip", n, build, hit, miss, iter);

    let mut f: FxMap<u64, u64> = FxMap::default();
    for (i, k) in ks.iter().enumerate() {
        f.insert(*k, i as u64);
    }
    let build = bench(|| {
        let mut x: FxMap<u64, u64> = FxMap::default();
        for (i, k) in ks.iter().enumerate() {
            x.insert(*k, i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *f.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *f.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(f.values().sum::<u64>());
    }, iters);
    report("HashMap/Fx", n, build, hit, miss, iter);

    let mut b: BTreeMap<u64, u64> = BTreeMap::new();
    for (i, k) in ks.iter().enumerate() {
        b.insert(*k, i as u64);
    }
    let build = bench(|| {
        let mut x: BTreeMap<u64, u64> = BTreeMap::new();
        for (i, k) in ks.iter().enumerate() {
            x.insert(*k, i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *b.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *b.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(b.values().sum::<u64>());
    }, iters);
    report("BTreeMap", n, build, hit, miss, iter);

    let mut sorted: Vec<(u64, u64)> = ks.iter().enumerate().map(|(i, k)| (*k, i as u64)).collect();
    sorted.sort_unstable_by_key(|e| e.0);
    let build = bench(|| {
        let mut x: Vec<(u64, u64)> = ks.iter().enumerate().map(|(i, k)| (*k, i as u64)).collect();
        x.sort_unstable_by_key(|e| e.0);
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += sorted.binary_search_by_key(k, |e| e.0).map(|i| sorted[i].1).unwrap_or(0);
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += sorted.binary_search_by_key(&miss_key, |e| e.0).map(|i| sorted[i].1).unwrap_or(0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(sorted.iter().map(|e| e.1).sum::<u64>());
    }, iters);
    report("sorted Vec", n, build, hit, miss, iter);
    println!();
}

fn run_name(n: usize) {
    let ks = keys_name(n);
    let iters = 200;
    let miss_key = String::from("parts/char/FRM_9999.webp");

    let mut m: HashMap<String, u64> = HashMap::new();
    for (i, k) in ks.iter().enumerate() {
        m.insert(k.clone(), i as u64);
    }
    let build = bench(|| {
        let mut x: HashMap<String, u64> = HashMap::new();
        for (i, k) in ks.iter().enumerate() {
            x.insert(k.clone(), i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *m.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *m.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(m.values().sum::<u64>());
    }, iters);
    report("HashMap/Sip", n, build, hit, miss, iter);

    let mut f: FxMap<String, u64> = FxMap::default();
    for (i, k) in ks.iter().enumerate() {
        f.insert(k.clone(), i as u64);
    }
    let build = bench(|| {
        let mut x: FxMap<String, u64> = FxMap::default();
        for (i, k) in ks.iter().enumerate() {
            x.insert(k.clone(), i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *f.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *f.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(f.values().sum::<u64>());
    }, iters);
    report("HashMap/Fx", n, build, hit, miss, iter);

    let mut b: BTreeMap<String, u64> = BTreeMap::new();
    for (i, k) in ks.iter().enumerate() {
        b.insert(k.clone(), i as u64);
    }
    let build = bench(|| {
        let mut x: BTreeMap<String, u64> = BTreeMap::new();
        for (i, k) in ks.iter().enumerate() {
            x.insert(k.clone(), i as u64);
        }
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += *b.get(k).unwrap();
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += *b.get(&miss_key).unwrap_or(&0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(b.values().sum::<u64>());
    }, iters);
    report("BTreeMap", n, build, hit, miss, iter);

    let mut sorted: Vec<(String, u64)> = ks.iter().cloned().enumerate().map(|(i, k)| (k, i as u64)).collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let build = bench(|| {
        let mut x: Vec<(String, u64)> = ks.iter().cloned().enumerate().map(|(i, k)| (k, i as u64)).collect();
        x.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        black_box(x.len());
    }, 20);
    let hit = bench(|| {
        let mut s = 0u64;
        for k in &ks {
            s += sorted
                .binary_search_by(|e| e.0.as_str().cmp(k.as_str()))
                .map(|i| sorted[i].1)
                .unwrap_or(0);
        }
        black_box(s);
    }, iters);
    let miss = bench(|| {
        let mut s = 0u64;
        for _ in 0..n {
            s += sorted
                .binary_search_by(|e| e.0.as_str().cmp(miss_key.as_str()))
                .map(|i| sorted[i].1)
                .unwrap_or(0);
        }
        black_box(s);
    }, iters);
    let iter = bench(|| {
        black_box(sorted.iter().map(|e| e.1).sum::<u64>());
    }, iters);
    report("sorted Vec", n, build, hit, miss, iter);
    println!();
}

fn main() {
    println!("== u64 ids (dense-ish) ==");
    println!("(build = insert n keys; lookup-hit = n lookups of present keys; lookup-miss = n lookups of one absent key; iterate = one full pass)");
    for n in [8usize, 64, 512, 4096, 65536] {
        run_u64(n);
    }
    println!("== short String names (~23 bytes) ==");
    for n in [8usize, 64, 512, 4096, 65536] {
        run_name(n);
    }
}
