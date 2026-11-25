//! The member table: the keyed container every map in the runtime is built on.
//!
//! An object's members, a dictionary's keys and the debug map of live objects
//! are the same thing with different key and value types: a key and a value,
//! found by the key, enumerated in an order a script can observe. This module
//! is that container once — [`HashTable`], generic over the key and the value.
//!
//! The key is any [`TableKey`] type. The table never looks inside a key: it
//! asks the key for the hash it is filed under and compares two keys with the
//! key's own equality. A string key therefore needs nothing beyond those two
//! pieces of knowledge, and an integer or an object id is an ordinary key
//! beside it.
//!
//! # The order the entries are enumerated in
//!
//! Enumeration order is part of the compatibility surface: a member loop, a
//! dictionary written into a save file and a message table a script fills in
//! all see the order of the entries. The table therefore keeps an order list
//! of its live entries beside the buckets, and that list is the contract:
//!
//! * a new key is linked at the front of the list;
//! * writing a key that is already present replaces its value and moves the
//!   entry to the front, so the order is "most recently written first";
//! * removing an entry unlinks it wherever it stands and leaves the rest in
//!   place, so its neighbours close the gap;
//! * a key that is removed and then written again is a new entry, at the
//!   front;
//! * the bucket layout never enters the list, so where an entry is filed
//!   reorders nothing.
//!
//! [`HashTable::iter`] walks the entries from the front, which is the reverse
//! of the order in which their keys were last written. A consumer that needs
//! the oldest entry first walks the entries back to front.
//!
//! # The buckets
//!
//! The table files a key into `hash % BUCKET_COUNT` and has [`BUCKET_COUNT`]
//! buckets. A bucket holds its first entry and a chain of the entries that
//! followed it, and a chain has no length limit, so any number of keys fits
//! without the bucket array changing. A probe compares the stored hash before
//! the key, so a key that merely shares a bucket costs one integer comparison
//! rather than a key comparison.
//!
//! The bucket count, the hash values and the chain order are internal wiring.
//! No script-visible value depends on any of them: a lookup answers by key,
//! and the enumeration follows the order list, not the buckets.
//!
//! # The bookkeeping
//!
//! A link — a bucket head, a chain step, an order step — is a 32-bit index
//! into one arena of entries, and the arena is dense: every slot holds a live
//! entry, so a link can only name an entry that exists. There is no raw
//! pointer in the table, no allocation per link and no empty slot to test for.
//! A removal moves the arena's last entry into the freed slot and points every
//! link that named the last entry at the slot it now occupies; the slot an
//! entry lives in is not an identity and nothing outside the table sees it.
//!
//! # Why there is no order check
//!
//! The order list cannot fall out of step with the live entries, so there is
//! no check that walks the whole table and no repair that rebuilds the list. A
//! link is an index into the arena, so it names an entry that exists or it is
//! absent; the two ends of the list are fields of the table; and the links and
//! the two ends change in the same two operations, one that links a new entry
//! at the front and one that unlinks an entry, while the count is the arena's
//! own length. A walk would only re-derive what the type already maintains.

/// The number of buckets the table files keys into.
pub const BUCKET_COUNT: usize = 64;

/// A key of the table: an equality and the 32-bit hash the table files it under.
pub trait TableKey: Eq {
    /// The hash the table files this key under.
    fn table_hash(&self) -> u32;
}

impl TableKey for u32 {
    fn table_hash(&self) -> u32 {
        // A finalizer that spreads the bits of a small integer over the whole
        // word, so consecutive keys do not share a bucket.
        let mut hash = *self;
        hash ^= hash >> 16;
        hash = hash.wrapping_mul(0x7feb_352d);
        hash ^= hash >> 15;
        hash = hash.wrapping_mul(0x846c_a68b);
        hash ^= hash >> 16;
        hash
    }
}

impl TableKey for u64 {
    fn table_hash(&self) -> u32 {
        // A finalizer over the whole word, then the two halves folded into the
        // table's 32-bit hash so that a key differing only in its high half
        // still reaches another bucket.
        let mut hash = *self;
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        hash ^= hash >> 33;
        (hash ^ (hash >> 32)) as u32
    }
}

/// The link value that names no entry.
///
/// A link is an index into the arena, so the greatest index is the end of every
/// chain and of the order list.
const NO_ENTRY: u32 = u32::MAX;

/// One stored key and value, with the links that place it in the table.
///
/// The fields a chain step reads — the hash, the next chain link and the key —
/// are declared first and together, so a walk over a long bucket chain reads
/// one part of an entry.
struct Entry<K, V> {
    /// The hash the key was filed under, kept so a probe can reject a
    /// candidate without comparing the keys.
    hash: u32,
    /// The next entry of this bucket's chain, or `NO_ENTRY` at the end of it.
    chain: u32,
    /// The entry before it in the chain, or `NO_ENTRY` when the bucket's own
    /// link points at it.
    chain_prev: u32,
    /// The stored key.
    key: K,
    /// The stored value.
    value: V,
    /// The next entry in the order list, or `NO_ENTRY` towards the back.
    order_next: u32,
    /// The previous entry in the order list, or `NO_ENTRY` towards the front.
    order_prev: u32,
}

/// A table of keys and values, searched by hash and enumerated in the order
/// the entries were last written.
///
/// The key is any [`TableKey`] type; the value is any type. See the module
/// documentation for the enumeration order, the bucket policy and the
/// ownership of the entries.
pub struct HashTable<K, V> {
    /// One slot per bucket: the index of the bucket's first entry, or
    /// `NO_ENTRY` when the bucket is empty.
    buckets: [u32; BUCKET_COUNT],
    /// Every live entry; the arena has no empty slot.
    entries: Vec<Entry<K, V>>,
    /// The index of the entry the enumeration starts at, or `NO_ENTRY`.
    order_first: u32,
    /// The index of the entry the enumeration ends at, or `NO_ENTRY`.
    order_last: u32,
}

/// The bucket a key with `hash` is filed in.
fn bucket_of(hash: u32) -> usize {
    (hash as usize) % BUCKET_COUNT
}

impl<K, V> HashTable<K, V> {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self {
            buckets: [NO_ENTRY; BUCKET_COUNT],
            entries: Vec::new(),
            order_first: NO_ENTRY,
            order_last: NO_ENTRY,
        }
    }

    /// The number of entries in the table.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table holds no entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes every entry, keeping the bucket array at its size.
    pub fn clear(&mut self) {
        self.buckets = [NO_ENTRY; BUCKET_COUNT];
        self.entries.clear();
        self.order_first = NO_ENTRY;
        self.order_last = NO_ENTRY;
    }

    /// The live entries, most recently written first.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            table: self,
            next: self.order_first,
        }
    }

    /// The entry at `index`.
    ///
    /// A link that points past the end of the arena is a broken table, and the
    /// arena's own bounds check is the one test a link pays.
    fn entry(&self, index: u32) -> &Entry<K, V> {
        &self.entries[index as usize]
    }

    /// The entry at `index` for change.
    fn entry_mut(&mut self, index: u32) -> &mut Entry<K, V> {
        &mut self.entries[index as usize]
    }

    /// Links the entry at `index` at the front of the enumeration order.
    fn link_at_front(&mut self, index: u32) {
        let first = self.order_first;
        {
            let entry = self.entry_mut(index);
            entry.order_prev = NO_ENTRY;
            entry.order_next = first;
        }
        if first != NO_ENTRY {
            self.entry_mut(first).order_prev = index;
        } else {
            self.order_last = index;
        }
        self.order_first = index;
    }

    /// Unlinks the entry at `index` from the enumeration order.
    fn unlink_from_order(&mut self, index: u32) {
        let entry = self.entry(index);
        let previous = entry.order_prev;
        let next = entry.order_next;
        if previous != NO_ENTRY {
            self.entry_mut(previous).order_next = next;
        } else {
            self.order_first = next;
        }
        if next != NO_ENTRY {
            self.entry_mut(next).order_prev = previous;
        } else {
            self.order_last = previous;
        }
    }

    /// Moves the entry at `index` to the front of the enumeration order.
    fn move_to_front(&mut self, index: u32) {
        if self.order_first == index {
            return;
        }
        self.unlink_from_order(index);
        self.link_at_front(index);
    }

    /// Removes the entry at `index` and answers its key and value.
    ///
    /// The entry leaves its bucket chain and the enumeration order, and the
    /// arena's last entry takes the freed slot, so the arena stays dense and no
    /// link ever names an empty slot.
    fn take_entry(&mut self, index: u32) -> (K, V) {
        let (hash, chain, chain_prev, order_prev, order_next) = {
            let entry = self.entry(index);
            (
                entry.hash,
                entry.chain,
                entry.chain_prev,
                entry.order_prev,
                entry.order_next,
            )
        };
        if chain_prev != NO_ENTRY {
            self.entry_mut(chain_prev).chain = chain;
        } else {
            self.buckets[bucket_of(hash)] = chain;
        }
        if chain != NO_ENTRY {
            self.entry_mut(chain).chain_prev = chain_prev;
        }
        if order_prev != NO_ENTRY {
            self.entry_mut(order_prev).order_next = order_next;
        } else {
            self.order_first = order_next;
        }
        if order_next != NO_ENTRY {
            self.entry_mut(order_next).order_prev = order_prev;
        } else {
            self.order_last = order_prev;
        }
        let last = u32::try_from(self.entries.len() - 1).expect("an entry index fits a u32");
        if index != last {
            self.relocate(last, index);
        }
        let removed = self.entries.swap_remove(index as usize);
        (removed.key, removed.value)
    }

    /// Moves the entry at `from` to the slot at `to`, relinking every link that
    /// named it: its bucket or chain predecessor, its chain successor and its
    /// two order-list neighbours.
    fn relocate(&mut self, from: u32, to: u32) {
        let (hash, chain, chain_prev, order_prev, order_next) = {
            let entry = self.entry(from);
            (
                entry.hash,
                entry.chain,
                entry.chain_prev,
                entry.order_prev,
                entry.order_next,
            )
        };
        if chain_prev != NO_ENTRY {
            self.entry_mut(chain_prev).chain = to;
        } else {
            self.buckets[bucket_of(hash)] = to;
        }
        if chain != NO_ENTRY {
            self.entry_mut(chain).chain_prev = to;
        }
        if order_prev != NO_ENTRY {
            self.entry_mut(order_prev).order_next = to;
        } else {
            self.order_first = to;
        }
        if order_next != NO_ENTRY {
            self.entry_mut(order_next).order_prev = to;
        } else {
            self.order_last = to;
        }
    }
}

impl<K: TableKey, V> HashTable<K, V> {
    /// Stores `value` under `key` and answers the value the key held before.
    ///
    /// A key that is already present keeps its single entry: its value is
    /// replaced and the entry moves to the front of the enumeration order. A
    /// key that is absent is linked at the front as a new entry.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let hash = key.table_hash();
        if let Some(index) = self.find(&key, hash) {
            self.move_to_front(index);
            return Some(std::mem::replace(&mut self.entry_mut(index).value, value));
        }

        let bucket = bucket_of(hash);
        let head = self.buckets[bucket];
        let index = u32::try_from(self.entries.len()).expect("an entry index fits a u32");
        self.entries.push(Entry {
            hash,
            chain: head,
            chain_prev: NO_ENTRY,
            key,
            value,
            order_next: NO_ENTRY,
            order_prev: NO_ENTRY,
        });
        if head != NO_ENTRY {
            self.entry_mut(head).chain_prev = index;
        }
        self.buckets[bucket] = index;
        self.link_at_front(index);
        None
    }

    /// The value stored under `key`, or `None` when the key is absent.
    pub fn get(&self, key: &K) -> Option<&V> {
        let hash = key.table_hash();
        let index = self.find(key, hash)?;
        Some(&self.entry(index).value)
    }

    /// The value stored under `key` for change, or `None` when the key is
    /// absent.
    ///
    /// Changing the value in place does not move the entry: the enumeration
    /// order changes only on [`HashTable::insert`] and [`HashTable::remove`].
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let hash = key.table_hash();
        let index = self.find(key, hash)?;
        Some(&mut self.entry_mut(index).value)
    }

    /// Removes `key` and answers whether the key was present.
    ///
    /// The entry leaves the enumeration order wherever it stands, and its
    /// neighbour on either side takes the link the other way. Its value is
    /// dropped.
    pub fn remove(&mut self, key: &K) -> bool {
        let hash = key.table_hash();
        let Some(index) = self.find(key, hash) else {
            return false;
        };
        self.take_entry(index);
        true
    }

    /// Removes the oldest entry and answers its key and value.
    ///
    /// The oldest entry is the back of the enumeration order. `None` is
    /// returned when the table is empty.
    pub fn chop_last(&mut self) -> Option<(K, V)> {
        let index = self.order_last;
        if index == NO_ENTRY {
            return None;
        }
        Some(self.take_entry(index))
    }

    /// The index of the entry stored under `key` with `hash`, if the key is
    /// present.
    ///
    /// A step loads one entry and compares its hash. The arena holds no empty
    /// slot, so the load is the whole test: a link that points into the arena
    /// names a live entry, and a link that points past it is caught by the
    /// arena's own bounds check.
    fn find(&self, key: &K, hash: u32) -> Option<u32> {
        let mut index = self.buckets[bucket_of(hash)];
        while index != NO_ENTRY {
            let entry = self.entry(index);
            if entry.hash == hash && &entry.key == key {
                return Some(index);
            }
            index = entry.chain;
        }
        None
    }
}

impl<K, V> Default for HashTable<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// An iterator over a table's live entries, most recently written first.
pub struct Iter<'a, K, V> {
    /// The table the entries belong to.
    table: &'a HashTable<K, V>,
    /// The index of the entry the next call yields, or `NO_ENTRY`.
    next: u32,
}

impl<'a, K: 'a, V: 'a> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == NO_ENTRY {
            return None;
        }
        let entry = self.table.entry(self.next);
        self.next = entry.order_next;
        Some((&entry.key, &entry.value))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{BUCKET_COUNT, HashTable, TableKey, bucket_of};
    use crate::vm::runtime::string::TjsString;

    // Citations name reference/krkr2/cpp/core/tjs2/tjsHashSearch.h, whose
    // bucket array, chain, order list and deletion this table reproduces.
    // The bucket count is a capacity property: the table keeps the same fixed
    // bucket array and lets a bucket's chain grow, so no assertion below
    // depends on which bucket a key lands in.

    /// The keys of a table, in the order the enumeration yields them.
    fn keys(table: &HashTable<u32, u32>) -> Vec<u32> {
        table.iter().map(|(key, _)| *key).collect()
    }

    /// Two different keys filed in the same bucket.
    fn colliding_pair() -> (u32, u32) {
        let first = 0u32;
        let bucket = bucket_of(first.table_hash());
        let second = (1..10_000u32)
            .find(|key| bucket_of(key.table_hash()) == bucket)
            .expect("a bucket holds a second key among ten thousand keys");
        (first, second)
    }

    /// Two different keys with the same 32-bit hash.
    fn same_hash_pair() -> (u64, u64) {
        let mut seen: HashMap<u32, u64> = HashMap::new();
        for key in 0..1_000_000u64 {
            if let Some(previous) = seen.insert(key.table_hash(), key) {
                return (previous, key);
            }
        }
        panic!("a million keys hold two with the same 32-bit hash");
    }

    /// A 64-bit linear congruential generator: the model test needs a
    /// repeatable sequence, and the crate takes no random-number dependency.
    struct Lcg(u64);

    impl Lcg {
        /// The next value of the sequence, from the generator's high half.
        fn next_u32(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 32) as u32
        }
    }

    /// Stores a key in the model and answers the value it held.
    fn model_insert(model: &mut Vec<(u32, u64)>, key: u32, value: u64) -> Option<u64> {
        match model.iter().position(|(existing, _)| *existing == key) {
            Some(position) => {
                let (_, previous) = model.remove(position);
                model.insert(0, (key, value));
                Some(previous)
            }
            None => {
                model.insert(0, (key, value));
                None
            }
        }
    }

    /// Removes a key from the model and answers whether it was there.
    fn model_remove(model: &mut Vec<(u32, u64)>, key: u32) -> bool {
        match model.iter().position(|(existing, _)| *existing == key) {
            Some(position) => {
                model.remove(position);
                true
            }
            None => false,
        }
    }

    #[test]
    fn a_new_table_is_empty_with_the_sixty_four_buckets() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:20 — TJS_HS_DEFAULT_HASH_SIZE 64
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:151 — the bucket array is the
        // table's own storage, one element per bucket
        assert_eq!(BUCKET_COUNT, 64);
        let table: HashTable<u32, u32> = HashTable::new();
        assert_eq!(table.buckets.len(), BUCKET_COUNT);
        assert!(table.is_empty());
        assert!(table.iter().next().is_none());
    }

    #[test]
    fn a_stored_key_is_found_and_a_missing_key_is_not() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:232-237 — Add stores a key
        // and a value
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:295-303 — Find answers null
        // for a key the table does not hold
        let mut table: HashTable<u32, u32> = HashTable::new();
        assert_eq!(table.insert(7, 70), None);
        assert_eq!(table.get(&7), Some(&70));
        assert_eq!(table.get(&8), None);
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
    }

    #[test]
    fn writing_a_present_key_replaces_its_value_and_keeps_one_entry() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:271-280 — the bucket's first
        // element with the same key takes the value instead of adding
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:246-254 — the same for an
        // element further along the chain
        let mut table: HashTable<u32, u32> = HashTable::new();
        assert_eq!(table.insert(1, 10), None);
        assert_eq!(table.insert(1, 11), Some(10));
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&1), Some(&11));
    }

    #[test]
    fn the_enumeration_is_most_recently_written_first() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:507-518 — a new element is
        // linked in front of the list's first element
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        table.insert(3, 30);
        assert_eq!(keys(&table), vec![3, 2, 1]);
    }

    #[test]
    fn writing_a_present_key_moves_it_to_the_front() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:542-555 — an element whose
        // key is written again moves to the front of the order
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        table.insert(3, 30);
        table.insert(2, 21);
        assert_eq!(keys(&table), vec![2, 3, 1]);
        assert_eq!(table.get(&2), Some(&21));
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn removing_an_entry_closes_the_gap_it_leaves() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:530-535 — an intermediate
        // element's neighbours are linked to each other
        let mut table = HashTable::new();
        for key in 1..=4u32 {
            table.insert(key, key * 10);
        }
        assert!(table.remove(&2));
        assert_eq!(keys(&table), vec![4, 3, 1]);
        assert_eq!(table.get(&2), None);
    }

    #[test]
    fn removing_the_first_and_the_last_entry_moves_the_ends() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:523-529 — deleting the first
        // or the last element moves the list's end, not a neighbour's link
        let mut table = HashTable::new();
        for key in 1..=3u32 {
            table.insert(key, key * 10);
        }
        assert!(table.remove(&3));
        assert_eq!(keys(&table), vec![2, 1]);
        assert!(table.remove(&1));
        assert_eq!(keys(&table), vec![2]);
    }

    #[test]
    fn a_re_inserted_key_is_a_new_entry_at_the_front() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:260-268 — a bucket whose
        // element was deleted takes the next key as a fresh element
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:507-518 — a fresh element
        // goes to the front of the order
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        table.insert(3, 30);
        assert!(table.remove(&2));
        assert_eq!(keys(&table), vec![3, 1]);
        table.insert(2, 22);
        assert_eq!(keys(&table), vec![2, 3, 1]);
    }

    #[test]
    fn removal_reports_whether_the_key_was_there() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:384-387 — Delete answers
        // whether the key was deleted
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:421 — a chain that ends
        // without the key answers no
        let mut table: HashTable<u32, u32> = HashTable::new();
        table.insert(1, 10);
        assert!(!table.remove(&2));
        assert_eq!(table.len(), 1);
        assert!(table.remove(&1));
        assert!(!table.remove(&1));
        assert!(table.is_empty());
    }

    #[test]
    fn removing_the_last_entry_empties_the_order() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:536-539 — with the count at
        // zero the list's first and last element are both cleared
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        assert!(table.remove(&2));
        assert!(table.remove(&1));
        assert!(table.is_empty());
        assert!(table.iter().next().is_none());
        table.insert(3, 30);
        assert_eq!(keys(&table), vec![3]);
    }

    #[test]
    fn a_colliding_key_is_found_removed_and_written_again() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:243 — a key is filed in
        // the bucket its hash selects
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:282-292 — a key that
        // shares the bucket but not the key is chained into it
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:496-503 — a lookup is
        // the bucket's first element and then the chain
        let mut table: HashTable<u32, u32> = HashTable::new();
        let (first, second) = colliding_pair();
        table.insert(first, 10);
        table.insert(second, 20);
        assert_eq!(table.get(&first), Some(&10));
        assert_eq!(table.get(&second), Some(&20));
        assert!(table.remove(&first));
        assert_eq!(table.get(&first), None);
        assert_eq!(table.get(&second), Some(&20));
        table.insert(first, 11);
        assert_eq!(table.get(&first), Some(&11));
        assert_eq!(keys(&table), vec![first, second]);
    }

    #[test]
    fn two_keys_with_the_same_hash_are_two_entries() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:246-249 — an element is the
        // key only when the hash and the key both match
        let mut table: HashTable<u64, u32> = HashTable::new();
        let (first, second) = same_hash_pair();
        assert_eq!(first.table_hash(), second.table_hash());
        table.insert(first, 10);
        table.insert(second, 20);
        assert_eq!(table.len(), 2);
        assert_eq!(table.get(&first), Some(&10));
        assert_eq!(table.get(&second), Some(&20));
        assert!(table.remove(&first));
        assert_eq!(table.get(&first), None);
        assert_eq!(table.get(&second), Some(&20));
    }

    #[test]
    fn a_table_of_strings_stores_finds_replaces_and_removes() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:57-78 — a string key is
        // hashed over its code units
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:232-303 — the table
        // stores, finds and replaces by key
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:507-518 — a new key is
        // linked at the front of the order
        let mut table: HashTable<TjsString, u32> = HashTable::new();
        let name = TjsString::from("name");
        let length = TjsString::from("length");
        assert_eq!(table.insert(name.clone(), 1), None);
        assert_eq!(table.insert(length.clone(), 2), None);
        assert_eq!(table.get(&name), Some(&1));
        assert_eq!(table.get(&length), Some(&2));
        assert_eq!(table.len(), 2);
        let order: Vec<TjsString> = table.iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(order, vec![length.clone(), name.clone()]);
        assert_eq!(table.insert(name.clone(), 3), Some(1));
        assert_eq!(table.get(&name), Some(&3));
        let order: Vec<TjsString> = table.iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(order, vec![name.clone(), length.clone()]);
        assert!(table.remove(&length));
        assert_eq!(table.get(&length), None);
        assert_eq!(table.len(), 1);
        let order: Vec<TjsString> = table.iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(order, vec![name]);
    }

    #[test]
    fn changing_a_value_in_place_keeps_the_order() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:295-303 — a plain find reads
        // the value and leaves the order alone
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:340-348 — only a find that
        // touches moves the element to the front
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        *table.get_mut(&1).expect("the key is present") = 11;
        assert_eq!(keys(&table), vec![2, 1]);
        assert_eq!(table.get(&1), Some(&11));
        assert_eq!(table.get_mut(&3), None);
    }

    #[test]
    fn clearing_empties_the_table_and_keeps_its_buckets() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:226 — Clear removes every
        // element
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:439-465 — the table is
        // initialised again at its own bucket count
        let mut table: HashTable<u32, u32> = HashTable::new();
        for key in 0..200 {
            table.insert(key, key);
        }
        table.clear();
        assert!(table.is_empty());
        assert!(table.iter().next().is_none());
        assert_eq!(table.buckets.len(), BUCKET_COUNT);
        assert_eq!(table.get(&7), None);
        table.insert(7, 7);
        assert_eq!(keys(&table), vec![7]);
    }

    #[test]
    fn chopping_takes_the_oldest_entry_off_the_back() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:424-434 — ChopLast
        // removes entries from the back of the order, oldest first
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:467-482 — one element is
        // unlinked from the order and from its chain
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:586-591 — the cache
        // chops the oldest entries once it passes its maximum count
        let mut table = HashTable::new();
        table.insert(1, 10);
        table.insert(2, 20);
        table.insert(3, 30);
        assert_eq!(table.chop_last(), Some((1, 10)));
        assert_eq!(table.chop_last(), Some((2, 20)));
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&3), Some(&30));
        assert_eq!(keys(&table), vec![3]);
        assert_eq!(table.chop_last(), Some((3, 30)));
        assert_eq!(table.chop_last(), None);
        assert!(table.is_empty());
    }

    #[test]
    fn a_table_of_many_keys_finds_every_key_and_keeps_the_order() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:490-504 — a lookup is the
        // bucket's first element and then its chain
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:507-518 — the order list does
        // not depend on the bucket a key is filed in
        let mut table: HashTable<u32, u32> = HashTable::new();
        for key in 0..200 {
            table.insert(key, key * 3);
        }
        assert_eq!(table.buckets.len(), BUCKET_COUNT);
        assert_eq!(table.len(), 200);
        let mut expected: Vec<u32> = (0..200).rev().collect();
        assert_eq!(keys(&table), expected);
        for key in 0..200 {
            assert_eq!(table.get(&key), Some(&(key * 3)));
        }
        assert!(table.remove(&150));
        assert_eq!(table.get(&150), None);
        table.insert(150, 1);
        expected.retain(|key| *key != 150);
        expected.insert(0, 150);
        assert_eq!(keys(&table), expected);
        assert_eq!(table.buckets.len(), BUCKET_COUNT);
    }

    #[test]
    fn the_table_matches_an_ordered_model_over_random_operations() {
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:232-292 — an absent key is a
        // new element and a present key keeps its one element
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:507-518 — a new element is
        // linked at the front
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:542-555 — an element whose
        // key is written again moves to the front
        // reference/krkr2/cpp/core/tjs2/tjsHashSearch.h:384-422 — a delete unlinks
        // the element and reports whether the key was there
        const OPERATIONS: usize = 10_000;
        let mut table: HashTable<u32, u64> = HashTable::new();
        let mut model: Vec<(u32, u64)> = Vec::new();
        let mut random = Lcg(0x5eed_1234_9abc_def1);

        for _ in 0..OPERATIONS {
            let key = random.next_u32() % 96;
            match random.next_u32() % 4 {
                0 | 1 => {
                    let value = u64::from(random.next_u32());
                    let previous = model_insert(&mut model, key, value);
                    assert_eq!(table.insert(key, value), previous);
                }
                2 => {
                    let removed = model_remove(&mut model, key);
                    assert_eq!(table.remove(&key), removed);
                }
                _ => {
                    let expected = model
                        .iter()
                        .find(|(existing, _)| *existing == key)
                        .map(|(_, value)| value);
                    assert_eq!(table.get(&key), expected);
                }
            }
            assert_eq!(table.len(), model.len());
            assert_eq!(table.is_empty(), model.is_empty());
            for (key, value) in &model {
                assert_eq!(table.get(key), Some(value));
            }
            let actual: Vec<(u32, u64)> = table.iter().map(|(key, value)| (*key, *value)).collect();
            assert_eq!(actual, model);
        }
    }
}
