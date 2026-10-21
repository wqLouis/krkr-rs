//! The crate's hash containers.
//!
//! The maps and sets are `hashbrown`'s, keyed with `ahash`. A table whose
//! iteration order reaches a script — the sound, video and movie dispatch
//! tables — is built with [`hasher`], whose fixed seeds keep the order the
//! same in every run; the other tables use `ahash`'s randomized state.

use ahash::RandomState;
use hashbrown::{HashMap, HashSet};

/// A map keyed with `ahash`.
pub(crate) type Map<K, V> = HashMap<K, V, RandomState>;

/// A set keyed with `ahash`.
pub(crate) type Set<T> = HashSet<T, RandomState>;

/// The fixed-seed hasher for the tables whose iteration order reaches a
/// script.
pub(crate) fn hasher() -> RandomState {
    RandomState::with_seeds(
        0x1d_8e_4f_27_3a_95_b0,
        0x6b_c2_07_5e_14_9d_38,
        0x95_f1_30_8a_4d_6c_72,
        0xe0_7b_63_1f_2a_85_d4,
    )
}
