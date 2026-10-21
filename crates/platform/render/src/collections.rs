//! The crate's hash containers.
//!
//! The maps and sets are `hashbrown`'s, keyed with `ahash`. Every table in
//! this crate is a lookup table or a resource registry whose iteration order
//! does not reach a caller, so `ahash`'s randomized state is enough.

use ahash::RandomState;
use hashbrown::{HashMap, HashSet};

/// A map keyed with `ahash`.
pub(crate) type Map<K, V> = HashMap<K, V, RandomState>;

/// A set keyed with `ahash`.
pub(crate) type Set<T> = HashSet<T, RandomState>;

/// An empty set with room for `capacity` entries.
pub(crate) fn set_with_capacity<T>(capacity: usize) -> Set<T> {
    Set::with_capacity_and_hasher(capacity, RandomState::default())
}
