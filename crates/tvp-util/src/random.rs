//! The TVP pseudo random generator, ported from `utils/Random.cpp`.
//!
//! # The reference algorithm (read before changing anything here)
//!
//! `Random.cpp` does **not** use an LCG or MT. It implements a simple
//! *environment-noise driven* generator built on an MD5 hash of a 4 KiB
//! seed pool:
//!
//! - a global 4 KiB pool (`TVPRandomSeedPool`) plus a position cursor
//!   (`TVPRandomSeedPoolPos`) and a running "atom" byte
//!   (`TVPRandomSeedAtom`);
//! - [`Random::push_noise`] (`TVPPushEnvironNoise`) mixes arbitrary bytes
//!   into the pool;
//! - [`Random::get_random_bits_128`] (`TVPGetRandomBits128`) pushes fresh
//!   noise, hashes the whole pool with MD5, returns the 16-byte digest, and
//!   pushes the digest back into the pool.
//!
//! The C++ seeds the pool by pushing *uninitialized stack bytes* — the
//! sequence is intentionally **non-deterministic**, so no save game or
//! script can ever depend on a reproducible stream. There is therefore
//! nothing save-game-compatible to preserve, and no reason to pull in the
//! `rand` crate; the mixing structure itself is what is ported faithfully.
//!
//! # Rust-side deviations (all documented)
//!
//! - Reading uninitialized memory is UB in Rust, so the "uninitialized
//!   stack buffer" pushes are replaced by (a) a per-instance monotonic
//!   counter (deterministic given prior calls) and (b) process entropy
//!   (time / ASLR / pid / thread id) gathered in [`Random::new`].
//! - [`Random::with_seed`] is a krkr-rs extension (the C++ has no seeding
//!   API) that expands a `u64` seed with splitmix64 to make streams
//!   reproducible for tests, replays and debugging. It does **not** exist
//!   in the reference.
//! - The pool is per-instance state instead of C++ global state. A global
//!   [`Random::global`] accessor mirrors the reference globals for engine
//!   code that wants them.
//!
//! This is not a cryptographically secure generator; it is a cheap mixer,
//! exactly like the reference ("may not be suitable for high security
//! usage").

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};

/// Size of the seed pool, matching `TVPRandomSeedPool`'s hashed 0x1000
/// bytes (the C++ allocates an extra 512 slack bytes for multi-threaded
/// overruns, which a `&mut self` API does not need).
pub const POOL_SIZE: usize = 0x1000;

/// `TVPRandomSeedPoolPos &= 0xfff` wraps the cursor back into the pool.
const POOL_MASK: usize = POOL_SIZE - 1;

/// The TVP environment-noise pseudo random generator.
///
/// See the [module docs](self) for the algorithm and deviations.
#[derive(Debug)]
pub struct Random {
    /// 4 KiB seed pool (the hashed state).
    pool: Box<[u8; POOL_SIZE]>,
    /// `TVPRandomSeedPoolPos`.
    pos: usize,
    /// `TVPRandomSeedAtom` — running xor byte.
    atom: u8,
    /// krkr-rs extension: deterministic per-call noise (replaces the C++
    /// uninitialized stack buffer pushes).
    counter: u64,
    /// Cache of unused bytes from the last `get_random_bits_128` call, so
    /// `next_u32`/`next_u64` do not re-hash per value.
    cache: [u8; 16],
    cache_start: usize,
    cache_end: usize,
}

impl Default for Random {
    fn default() -> Self {
        Self::new()
    }
}

impl Random {
    /// Creates a generator seeded from process entropy (time, ASLR stack
    /// address, pid, thread id). The C++ equivalent pushes uninitialized
    /// stack memory; this is the UB-free stand-in, and like the reference
    /// it is not cryptographic.
    pub fn new() -> Self {
        let mut rng = Self::empty();
        let mut entropy = [0u8; 64];

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        entropy[0..8].copy_from_slice(&now.to_le_bytes());

        // ASLR-ish entropy: the address of our own stack buffer.
        let stack_addr = &entropy as *const _ as usize;
        entropy[8..16].copy_from_slice(&(stack_addr as u64).to_le_bytes());

        entropy[16..24].copy_from_slice(&(std::process::id() as u64).to_le_bytes());

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        entropy[24..32].copy_from_slice(&hasher.finish().to_le_bytes());

        rng.push_noise(&entropy);
        rng
    }

    /// Creates a generator whose stream is fully deterministic: `seed` is
    /// expanded with splitmix64 and pushed through the same mixer the C++
    /// uses. **krkr-rs extension** — the reference has no seed API (its
    /// stream is intentionally non-deterministic), so this is only for
    /// tests, replays and debugging; never rely on it for save-game
    /// compatibility.
    pub fn with_seed(seed: u64) -> Self {
        let mut rng = Self::empty();
        let mut s = seed;
        for _ in 0..64 {
            s = splitmix64(s);
            rng.push_noise(&s.to_le_bytes());
        }
        rng.push_noise(&seed.to_le_bytes());
        rng
    }

    /// Zeroed pool, zero cursor/atom/counter — internal constructor used by
    /// [`Random::new`] and [`Random::with_seed`].
    fn empty() -> Self {
        Self {
            pool: Box::new([0; POOL_SIZE]),
            pos: 0,
            atom: 0,
            counter: 0,
            cache: [0; 16],
            cache_start: 0,
            cache_end: 0,
        }
    }

    /// Mixes `buf` into the seed pool — a faithful port of
    /// `TVPPushEnvironNoise`.
    ///
    /// The C++ unconditionally reads `p[0]` to decide whether to advance
    /// the cursor one extra byte, even for an empty buffer (UB); we treat an
    /// empty buffer as `p[0] == 0` and skip the adjustment.
    pub fn push_noise(&mut self, buf: &[u8]) {
        for &b in buf {
            self.atom ^= b;
            self.pool[self.pos] ^= self.atom;
            self.pos = (self.pos + 1) & POOL_MASK;
        }
        self.pos = (self.pos + (buf.first().copied().unwrap_or(0) as usize & 1)) & POOL_MASK;
    }

    /// Retrieves 128 random bits — a faithful port of
    /// `TVPGetRandomBits128`: mix fresh noise + the cursor, MD5-hash the
    /// 4 KiB pool, return the digest, and mix the digest back in.
    ///
    /// The "fresh noise" pushed before hashing replaces the C++'s
    /// uninitialized stack buffer with a deterministic per-instance counter
    /// (see the [module docs](self)).
    pub fn get_random_bits_128(&mut self) -> [u8; 16] {
        self.push_noise(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);

        // The C++ also pushes the pool cursor itself.
        self.push_noise(&(self.pos as u32).to_le_bytes());

        let mut hasher = Md5::new();
        hasher.update(*self.pool);
        let mut digest = [0u8; 16];
        digest.copy_from_slice(&hasher.finalize());

        // Push the hash itself, like the reference.
        self.push_noise(&digest);
        digest
    }

    /// Fills `out` with random bytes (multiple `get_random_bits_128` calls
    /// as needed).
    pub fn fill_bytes(&mut self, out: &mut [u8]) {
        let mut chunks = out.chunks_exact_mut(16);
        for chunk in &mut chunks {
            chunk.copy_from_slice(&self.get_random_bits_128());
        }
        let remainder = chunks.into_remainder();
        if !remainder.is_empty() {
            let digest = self.get_random_bits_128();
            remainder.copy_from_slice(&digest[..remainder.len()]);
        }
    }

    /// Random `u32` (little-endian from the 128-bit stream).
    pub fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.read_cache(&mut b);
        u32::from_le_bytes(b)
    }

    /// Random `u64` (little-endian from the 128-bit stream).
    pub fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.read_cache(&mut b);
        u64::from_le_bytes(b)
    }

    /// Uniform `f64` in `[0, 1)` (53 bits of mantissa).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// True with probability 0.5.
    pub fn next_bool(&mut self) -> bool {
        self.next_u32() & 1 == 1
    }

    /// Drains bytes from the per-instance cache, refilling it from
    /// `get_random_bits_128` when exhausted.
    fn read_cache(&mut self, out: &mut [u8]) {
        let mut filled = 0;
        while filled < out.len() {
            if self.cache_start == self.cache_end {
                self.cache = self.get_random_bits_128();
                self.cache_start = 0;
                self.cache_end = self.cache.len();
            }
            let n = (self.cache_end - self.cache_start).min(out.len() - filled);
            out[filled..filled + n]
                .copy_from_slice(&self.cache[self.cache_start..self.cache_start + n]);
            self.cache_start += n;
            filled += n;
        }
    }

    /// The process-global generator, mirroring the reference's global
    /// `TVPRandomSeedPool` state. Engine code that wants the C++-style
    /// "one shared pool" behavior can lock this.
    pub fn global() -> &'static Mutex<Random> {
        static GLOBAL: OnceLock<Mutex<Random>> = OnceLock::new();
        GLOBAL.get_or_init(|| Mutex::new(Random::new()))
    }
}

/// `TVPPushEnvironNoise` on the global generator.
pub fn push_environ_noise(buf: &[u8]) {
    Random::global()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push_noise(buf);
}

/// `TVPGetRandomBits128` on the global generator.
pub fn get_random_bits_128() -> [u8; 16] {
    Random::global()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_random_bits_128()
}

/// splitmix64 — a small, well-known 64-bit mixing function used only to
/// expand [`Random::with_seed`] seeds into pool noise. Not part of the
/// reference algorithm.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// Formats raw digest bytes as lowercase hex without re-hashing
    /// (`crate::md5::hex` would hash its input again).
    fn hex_bytes(d: &[u8; 16]) -> String {
        let mut s = String::with_capacity(32);
        for b in d {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    /// Expected values computed by simulating the C++ algorithm exactly
    /// (md5 of the pool) with an independent Python implementation.
    const CPP_REFERENCE_SEQUENCE: [&str; 5] = [
        "5b63860fa91ea62931c4b46c65d32310",
        "0ef4482a976514a1537c98575cc12efb",
        "ec1ffbb69dc3d3170bd8a4c17b08c645",
        "2b434a7de7bc0443db4593c2f5921f2d",
        "5c3be055526d253877bba33093b1610f",
    ];

    #[test]
    fn push_noise_matches_cpp_mixing() {
        // Zeroed pool, then push b"abc": the C++ mixer must produce this
        // exact pool (verified against an independent Python port).
        let mut rng = Random::empty();
        rng.push_noise(b"abc");
        assert_eq!(rng.pos, 4);

        let mut hasher = Md5::new();
        hasher.update(*rng.pool);
        let mut pool_digest = [0u8; 16];
        pool_digest.copy_from_slice(&hasher.finalize());
        assert_eq!(hex_bytes(&pool_digest), "0fb1db73bfce67d699d34fb9e2c6f43c");
    }

    #[test]
    fn seeded_sequence_matches_cpp_reference() {
        let mut rng = Random::with_seed(0x1234_5678_9abc_def0);
        for expected in CPP_REFERENCE_SEQUENCE {
            assert_eq!(hex_bytes(&rng.get_random_bits_128()), expected);
        }
    }

    #[test]
    fn different_seeds_give_different_streams() {
        let mut a = Random::with_seed(1);
        let mut b = Random::with_seed(2);
        assert_ne!(a.get_random_bits_128(), b.get_random_bits_128());
    }

    #[test]
    fn fill_bytes_matches_repeated_digests() {
        let mut rng = Random::with_seed(42);
        let first = rng.get_random_bits_128();
        let second = rng.get_random_bits_128();

        let mut rng2 = Random::with_seed(42);
        let mut buf = [0u8; 16];
        rng2.fill_bytes(&mut buf);
        assert_eq!(buf, first, "fill_bytes drains digests in order");
        assert_ne!(first, second, "the stream advances between calls");
    }

    #[test]
    fn next_u64_uses_cache_consistently() {
        // next_u64 through the cache must equal draining fill_bytes.
        let mut rng = Random::with_seed(7);
        let a = rng.next_u64();
        let b = rng.next_u64();
        let mut rng2 = Random::with_seed(7);
        let mut buf = [0u8; 16];
        rng2.fill_bytes(&mut buf);
        assert_eq!(a, u64::from_le_bytes(buf[0..8].try_into().unwrap()));
        assert_eq!(b, u64::from_le_bytes(buf[8..16].try_into().unwrap()));
    }

    #[test]
    fn next_f64_in_unit_interval() {
        let mut rng = Random::with_seed(99);
        for _ in 0..1000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn new_streams_are_distinct() {
        let mut a = Random::new();
        let mut b = Random::new();
        assert_ne!(a.get_random_bits_128(), b.get_random_bits_128());
    }

    #[test]
    fn statistical_sanity() {
        // Mean of next_u64 should be near u64::MAX/2 (loose 10% band).
        let mut rng = Random::with_seed(1234);
        const N: usize = 100_000;
        let mut sum = 0.0f64;
        for _ in 0..N {
            sum += rng.next_u64() as f64;
        }
        let mean = sum / N as f64;
        let expected = u64::MAX as f64 / 2.0;
        assert!(
            (mean - expected).abs() < expected * 0.10,
            "mean {mean} too far from {expected}"
        );
    }

    #[test]
    fn global_mirrors_cpp_global_api() {
        push_environ_noise(b"some environment noise");
        let bits = get_random_bits_128();
        assert_ne!(bits, [0u8; 16]);
    }

    #[test]
    fn empty_push_is_a_noop() {
        let mut a = Random::with_seed(5);
        let mut b = Random::with_seed(5);
        a.push_noise(b"");
        assert_eq!(a.get_random_bits_128(), b.get_random_bits_128());
    }
}
