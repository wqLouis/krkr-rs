//! Fixed-seed ahash maps for the engine's lookup tables.

/// A `HashMap` keyed with `ahash`'s `RandomState`.
pub type Map<K, V> = hashbrown::HashMap<K, V, ahash::RandomState>;

/// A `HashSet` keyed with `ahash`'s `RandomState`.
pub(crate) type Set<T> = hashbrown::HashSet<T, ahash::RandomState>;

/// The engine's hasher. Fixed seeds keep hashing deterministic across runs;
/// `ahash`'s default `RandomState` is randomized per process.
pub(crate) fn hasher() -> ahash::RandomState {
    ahash::RandomState::with_seeds(
        0x51_7c_c1_b7_27_22_0a_95,
        0x9e_37_79_b9_7f_4a_7c_15,
        0x24_5f_8d_3c_44_2a_1d_02,
        0xd6_e5_23_9b_6c_4a_1f_87,
    )
}

/// An empty map using [`hasher`].
pub(crate) fn map<K, V>() -> Map<K, V> {
    Map::with_hasher(hasher())
}

/// An empty set using [`hasher`].
pub(crate) fn set<T>() -> Set<T> {
    Set::with_hasher(hasher())
}
