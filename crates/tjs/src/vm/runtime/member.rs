//! The member map of a script object.
//!
//! A script object keeps its members in a symbol array of its own, not in the
//! keyed table the message and script caches use. The array has a
//! power-of-two bucket count, each bucket holds one primary member in the
//! bucket's own slot and a chain of the members that followed it, and the
//! array grows by four when an insertion finds it full. This module is that
//! storage.
//!
//! # The order members are enumerated in
//!
//! Enumeration is the part a script can see: it is the order a dictionary
//! writes its pairs into a save file. The walk is fixed by the array and not
//! by insertion time:
//!
//! * the buckets are visited from the first to the last;
//! * inside a bucket the chain is visited from its head, and the primary slot
//!   comes after the whole chain.
//!
//! A member that arrives while its bucket's primary slot is free takes that
//! slot, so it is enumerated after the chain; a member that arrives while the
//! slot is taken joins the head of the chain, so it is enumerated before the
//! chain. The members sharing a bucket are therefore not in insertion order:
//! the chain is the reverse of the order its members joined it, and the
//! primary is the first member that ever took the slot.
//!
//! A lookup reorders as well: a member found more than two places down its
//! chain moves to the chain's head. The chain links are cells for that reason
//! only — a read has to move the member it found — and enumeration never
//! moves anything.
//!
//! # The growth
//!
//! The array grows when an inserted name is absent and the array is full, and
//! the new bucket count is computed from the member count: eight buckets at
//! the start, then thirty-two, then a hundred and twenty-eight, four times the
//! old count each time. Growing re-files every member: the old array is walked
//! in the enumeration order above and each member is placed in the new array
//! by the rule an insertion uses, so the enumeration order after a growth is
//! the order of the new buckets, not the order of the old ones.

use std::cell::Cell;

use super::string::TjsString;

/// The bucket-count exponent a map starts with: eight buckets.
const START_BITS: u32 = 3;

/// The greatest bucket-count exponent the growth reaches.
const BUCKET_BITS_LIMIT: u32 = 32;

/// The link value that names no member.
const NO_MEMBER: u32 = u32::MAX;

/// One stored member.
struct Member<V> {
    /// The hash the name is filed under.
    hash: u32,
    /// The member's name.
    name: TjsString,
    /// The member's value.
    value: V,
    /// The next member of this bucket's chain, or `NO_MEMBER` at the end.
    ///
    /// The link is a cell because a lookup moves the member it found to the
    /// head of its chain, and a lookup borrows the map shared.
    next: Cell<u32>,
}

impl<V> Member<V> {
    /// Whether this member is the one `hash` and `name` denote.
    #[inline]
    fn matches(&self, hash: u32, name: &TjsString) -> bool {
        self.hash == hash && self.name.as_utf16() == name.as_utf16()
    }
}

/// One bucket of the symbol array.
struct Bucket<V> {
    /// The bucket's primary member, while its slot is taken.
    primary: Option<Member<V>>,
    /// The head of the bucket's chain, or `NO_MEMBER` when the chain is empty.
    chain: Cell<u32>,
}

impl<V> Bucket<V> {
    /// An empty bucket.
    fn new() -> Self {
        Self {
            primary: None,
            chain: Cell::new(NO_MEMBER),
        }
    }
}

/// Where a lookup found a member.
#[derive(Clone, Copy)]
enum Found {
    /// The primary slot of the bucket at the index.
    Primary(usize),
    /// A member of a bucket's chain.
    Chain {
        /// The link that points at the member, or `None` when it is the head.
        previous: Option<u32>,
        /// The member's slot in the overflow arena.
        index: u32,
        /// How many members lie between the chain's head and the member.
        distance: usize,
    },
}

/// The members of one script object, in the object's enumeration order.
///
/// The name is the member's key and its hash is the one the object files the
/// name under; the value is any type the caller stores. See the module
/// documentation for the enumeration order and the growth of the array.
pub struct MemberMap<V> {
    /// The symbol array: one bucket per slot, a power of two of them.
    buckets: Vec<Bucket<V>>,
    /// Every member that is not a bucket's primary, in the arena's own order.
    overflow: Vec<Option<Member<V>>>,
    /// The slots of the overflow arena that hold no member.
    free: Vec<u32>,
    /// The number of members.
    len: usize,
}

impl<V> MemberMap<V> {
    /// A map holding no member.
    pub fn new() -> Self {
        Self {
            buckets: (0..(1usize << START_BITS)).map(|_| Bucket::new()).collect(),
            overflow: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }

    /// The number of members.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the map holds no member.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Stores `value` under `name` and answers the value the name held before.
    ///
    /// A name that is already present keeps the slot its member already has:
    /// only the value changes. A name that is absent joins the array as the
    /// primary of its bucket when that slot is free and at the head of its
    /// bucket's chain otherwise; the array grows by four first when it is full.
    pub fn insert(&mut self, name: TjsString, value: V) -> Option<V> {
        let hash = name.member_hash();
        if let Some(found) = self.find(hash, &name) {
            return Some(std::mem::replace(self.value_mut(found), value));
        }
        if self.len >= self.buckets.len() {
            self.grow();
        }
        self.place(Member {
            hash,
            name,
            value,
            next: Cell::new(NO_MEMBER),
        });
        self.len += 1;
        None
    }

    /// The value stored under `name`, or `None` when the name is absent.
    ///
    /// Finding the member moves it to the head of its chain when it lies more
    /// than two places down it, so a read can change the enumeration order;
    /// that is the lookup's own rule and it applies to every lookup.
    pub fn get(&self, name: &TjsString) -> Option<&V> {
        let hash = name.member_hash();
        let found = self.find(hash, name)?;
        Some(self.value(found))
    }

    /// The value stored under `name` for change, or `None` when the name is
    /// absent.
    ///
    /// Finding the member moves it to the head of its chain when it lies more
    /// than two places down it, so the enumeration order can change.
    pub fn get_mut(&mut self, name: &TjsString) -> Option<&mut V> {
        let hash = name.member_hash();
        let found = self.find(hash, name)?;
        Some(self.value_mut(found))
    }

    /// Removes `name` and answers whether it was present.
    ///
    /// A primary member leaves its slot empty and its bucket's chain standing:
    /// the chain never hung off the member. A member of the chain is unlinked
    /// where it stands.
    pub fn remove(&mut self, name: &TjsString) -> bool {
        let hash = name.member_hash();
        let bucket = self.bucket_of(hash);
        let is_primary = self.buckets[bucket]
            .primary
            .as_ref()
            .is_some_and(|primary| primary.matches(hash, name));
        if is_primary {
            self.buckets[bucket].primary = None;
            self.len -= 1;
            return true;
        }
        let mut previous = NO_MEMBER;
        let mut index = self.buckets[bucket].chain.get();
        while index != NO_MEMBER {
            let member = self.overflow(index);
            let matches = member.matches(hash, name);
            let next = member.next.get();
            if matches {
                if previous == NO_MEMBER {
                    self.buckets[bucket].chain.set(next);
                } else {
                    self.overflow_mut(previous).next.set(next);
                }
                self.overflow[index as usize] = None;
                self.free.push(index);
                self.len -= 1;
                return true;
            }
            previous = index;
            index = next;
        }
        false
    }

    /// Removes every member, keeping the bucket count.
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = Bucket::new();
        }
        self.overflow.clear();
        self.free.clear();
        self.len = 0;
    }

    /// The members, in the object's enumeration order.
    pub fn iter(&self) -> Iter<'_, V> {
        Iter {
            map: self,
            bucket: 0,
            chain: self.buckets[0].chain.get(),
            primary: true,
        }
    }

    /// The bucket a name with `hash` is filed in.
    fn bucket_of(&self, hash: u32) -> usize {
        let mask = u32::try_from(self.buckets.len() - 1).expect("a bucket mask fits a u32");
        (hash & mask) as usize
    }

    /// The overflow member at `index`.
    fn overflow(&self, index: u32) -> &Member<V> {
        self.overflow[index as usize]
            .as_ref()
            .expect("a chain link names a member")
    }

    /// The overflow member at `index` for change.
    fn overflow_mut(&mut self, index: u32) -> &mut Member<V> {
        self.overflow[index as usize]
            .as_mut()
            .expect("a chain link names a member")
    }

    /// The value of the member a lookup found.
    fn value(&self, found: Found) -> &V {
        match found {
            Found::Primary(bucket) => {
                &self.buckets[bucket]
                    .primary
                    .as_ref()
                    .expect("a found member is present")
                    .value
            }
            Found::Chain { index, .. } => &self.overflow(index).value,
        }
    }

    /// The value of the member a lookup found, for change.
    fn value_mut(&mut self, found: Found) -> &mut V {
        match found {
            Found::Primary(bucket) => {
                &mut self.buckets[bucket]
                    .primary
                    .as_mut()
                    .expect("a found member is present")
                    .value
            }
            Found::Chain { index, .. } => &mut self.overflow_mut(index).value,
        }
    }

    /// Where the member filed under `name` with `hash` sits, if it is present.
    ///
    /// The chain is probed from its head and the primary slot after it, which
    /// is the order a lookup uses. A member found more than two places down
    /// the chain moves to the chain's head before the answer is returned.
    fn find(&self, hash: u32, name: &TjsString) -> Option<Found> {
        let bucket = self.bucket_of(hash);
        let mut previous = None;
        let mut distance = 0;
        let mut index = self.buckets[bucket].chain.get();
        while index != NO_MEMBER {
            let member = self.overflow(index);
            if member.matches(hash, name) {
                let found = Found::Chain {
                    previous,
                    index,
                    distance,
                };
                if let Found::Chain {
                    previous: Some(previous),
                    index,
                    distance,
                } = found
                    && distance > 2
                {
                    self.move_to_chain_head(bucket, previous, index);
                }
                return Some(found);
            }
            previous = Some(index);
            distance += 1;
            index = member.next.get();
        }
        match self.buckets[bucket].primary.as_ref() {
            Some(member) if member.matches(hash, name) => Some(Found::Primary(bucket)),
            _ => None,
        }
    }

    /// Moves the chain member at `index` to the head of its bucket's chain.
    ///
    /// `previous` is the member whose link points at it.
    fn move_to_chain_head(&self, bucket: usize, previous: u32, index: u32) {
        let next = self.overflow(index).next.get();
        self.overflow(previous).next.set(next);
        let head = self.buckets[bucket].chain.get();
        self.overflow(index).next.set(head);
        self.buckets[bucket].chain.set(index);
    }

    /// Files a member that the map does not hold yet.
    fn place(&mut self, mut member: Member<V>) {
        let bucket = self.bucket_of(member.hash);
        if self.buckets[bucket].primary.is_none() {
            member.next = Cell::new(NO_MEMBER);
            self.buckets[bucket].primary = Some(member);
        } else {
            member.next = Cell::new(self.buckets[bucket].chain.get());
            let index = self.push(member);
            self.buckets[bucket].chain.set(index);
        }
    }

    /// Puts a member in the overflow arena and answers its slot.
    fn push(&mut self, member: Member<V>) -> u32 {
        match self.free.pop() {
            Some(index) => {
                self.overflow[index as usize] = Some(member);
                index
            }
            None => {
                let index = u32::try_from(self.overflow.len()).expect("a member index fits a u32");
                self.overflow.push(Some(member));
                index
            }
        }
    }

    /// Grows the symbol array and re-files every member.
    ///
    /// The old array is walked in the enumeration order, and each member is
    /// filed in the new array by the rule an insertion uses.
    fn grow(&mut self) {
        let bits = bucket_bits(self.len);
        let size = 1usize
            .checked_shl(bits)
            .expect("a bucket count fits the address space");
        if size == self.buckets.len() {
            return;
        }
        let mut grown = Self {
            buckets: (0..size).map(|_| Bucket::new()).collect(),
            overflow: Vec::new(),
            free: Vec::new(),
            len: self.len,
        };
        let old = std::mem::take(&mut self.buckets);
        let mut old_overflow = std::mem::take(&mut self.overflow);
        for bucket in old {
            let mut index = bucket.chain.get();
            while index != NO_MEMBER {
                let member = old_overflow[index as usize]
                    .take()
                    .expect("a chain link names a member");
                index = member.next.get();
                grown.place(member);
            }
            if let Some(primary) = bucket.primary {
                grown.place(primary);
            }
        }
        *self = grown;
    }
}

impl<V> Default for MemberMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

/// The bucket-count exponent the growth applies at `count` members.
///
/// The exponent is the length of the count in binary plus one, so eight
/// members grow the array to thirty-two buckets and thirty-two grow it to a
/// hundred and twenty-eight.
fn bucket_bits(count: usize) -> u32 {
    let length = (usize::BITS - count.leading_zeros()).max(1);
    (length + 1).min(BUCKET_BITS_LIMIT)
}

/// An iterator over a map's members, in the object's enumeration order.
pub struct Iter<'a, V> {
    /// The map the members belong to.
    map: &'a MemberMap<V>,
    /// The bucket the walk is in.
    bucket: usize,
    /// The next member of that bucket's chain, or `NO_MEMBER`.
    chain: u32,
    /// Whether the bucket's primary slot is still to be visited.
    primary: bool,
}

impl<'a, V: 'a> Iterator for Iter<'a, V> {
    type Item = (&'a TjsString, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.chain != NO_MEMBER {
                let member = self.map.overflow(self.chain);
                self.chain = member.next.get();
                return Some((&member.name, &member.value));
            }
            if self.primary {
                self.primary = false;
                if let Some(member) = &self.map.buckets[self.bucket].primary {
                    return Some((&member.name, &member.value));
                }
            }
            self.bucket += 1;
            if self.bucket == self.map.buckets.len() {
                return None;
            }
            self.chain = self.map.buckets[self.bucket].chain.get();
            self.primary = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{c_char, c_void};
    use std::fs;
    use std::path::PathBuf;

    use super::MemberMap;
    use crate::vm::runtime::string::TjsString;
    use crate::{RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_RETAINED, VAL_VOID, Value};

    // Citations name reference/krkr2/cpp/core/tjs2/tjsObject.cpp unless the
    // line says otherwise; the entry that grows the array is in the shipped
    // copy the engine is built from, crates/tjs/cpp/tjs2/tjsObject.cpp.

    /// `count` names whose hashes share the first bucket of a fresh map.
    fn colliding_names(count: usize) -> Vec<TjsString> {
        let mut names = Vec::new();
        let mut index = 0;
        while names.len() < count {
            let name = TjsString::from(format!("member_{index}"));
            if name.member_hash() & 7 == 0 {
                names.push(name);
            }
            index += 1;
        }
        names
    }

    /// The keys of a map, in the order the enumeration yields them.
    fn keys(map: &MemberMap<u32>) -> Vec<TjsString> {
        map.iter().map(|(name, _)| name.clone()).collect()
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.h:241
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:335-337
    #[test]
    fn a_new_map_has_eight_buckets() {
        let map: MemberMap<u32> = MemberMap::new();
        assert_eq!(map.buckets.len(), 8);
        assert!(map.is_empty());
        assert_eq!(map.iter().count(), 0);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:654-676
    // crates/tjs/cpp/tjs2/tjsObject.cpp:525-526 — the shipped copy grows the
    // array when an insertion finds it full, before the member is added
    #[test]
    fn the_bucket_count_quadruples_at_the_growth_points() {
        let mut map: MemberMap<u32> = MemberMap::new();
        for index in 0..129u32 {
            let before = map.buckets.len();
            map.insert(TjsString::from(format!("key_{index}")), index);
            let expected = match map.len() {
                9 => 32,
                33 => 128,
                129 => 512,
                _ => before,
            };
            assert_eq!(map.buckets.len(), expected, "at {} members", map.len());
        }
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:520-530
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1094-1127
    #[test]
    fn a_colliding_member_joins_the_head_of_the_chain() {
        let names = colliding_names(3);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            assert_eq!(map.insert(name.clone(), index as u32), None);
        }
        // the first name took the primary slot; each later one arrived at the
        // head of the chain, so the chain is visited first, newest link first,
        // and the primary last
        assert_eq!(
            keys(&map),
            vec![names[2].clone(), names[1].clone(), names[0].clone()]
        );
        assert_eq!(map.len(), 3);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1094-1127
    #[test]
    fn the_buckets_are_visited_in_order() {
        // names in different buckets, so the walk is the bucket order alone
        let names: Vec<TjsString> = ["size", "left", "height", "b", "id", "a", "key1", "count"]
            .iter()
            .map(|name| TjsString::from(*name))
            .collect();
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        let mut expected = names.clone();
        expected.sort_by_key(|name| name.member_hash() & 7);
        assert_eq!(keys(&map), expected);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:770-776
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:505-512
    #[test]
    fn removing_the_primary_keeps_its_chain() {
        let names = colliding_names(3);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        assert!(map.remove(&names[0]));
        assert_eq!(keys(&map), vec![names[2].clone(), names[1].clone()]);
        // the next arrival takes the free primary slot, so it is enumerated
        // after the chain rather than at its head
        assert_eq!(map.insert(names[0].clone(), 9), None);
        assert_eq!(
            keys(&map),
            vec![names[2].clone(), names[1].clone(), names[0].clone()]
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:780-795
    #[test]
    fn removing_a_chain_member_relinks_the_chain() {
        let names = colliding_names(3);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        // the middle of the chain
        assert!(map.remove(&names[1]));
        assert_eq!(keys(&map), vec![names[2].clone(), names[0].clone()]);
        // the head of the chain
        assert!(map.remove(&names[2]));
        assert_eq!(keys(&map), vec![names[0].clone()]);
        assert_eq!(map.len(), 1);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:775
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:798
    #[test]
    fn removing_a_name_that_is_absent_answers_no() {
        let names = colliding_names(2);
        let mut map: MemberMap<u32> = MemberMap::new();
        map.insert(names[0].clone(), 0);
        assert!(!map.remove(&names[1]));
        assert_eq!(map.len(), 1);
        assert!(map.remove(&names[0]));
        assert!(!map.remove(&names[0]));
        assert!(map.is_empty());
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:493-510
    #[test]
    fn writing_a_present_name_keeps_its_slot() {
        let names = colliding_names(3);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        assert_eq!(map.insert(names[1].clone(), 99), Some(1));
        assert_eq!(map.get(&names[1]), Some(&99));
        assert_eq!(map.len(), 3);
        assert_eq!(
            keys(&map),
            vec![names[2].clone(), names[1].clone(), names[0].clone()]
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1035-1046
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1134
    #[test]
    fn a_read_moves_a_member_found_deep_in_its_chain() {
        let names = colliding_names(5);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        // the chain is names[4] .. names[1] and the primary is names[0];
        // names[1] is the fourth member of the chain, three places down, so
        // reading it moves it to the head
        assert_eq!(map.get(&names[1]), Some(&1));
        assert_eq!(
            keys(&map),
            vec![
                names[1].clone(),
                names[4].clone(),
                names[3].clone(),
                names[2].clone(),
                names[0].clone(),
            ]
        );
        // a member at the head of the chain and the primary stay where they
        // are
        assert_eq!(map.get(&names[1]), Some(&1));
        assert_eq!(map.get(&names[0]), Some(&0));
        assert_eq!(
            keys(&map),
            vec![
                names[1].clone(),
                names[4].clone(),
                names[3].clone(),
                names[2].clone(),
                names[0].clone(),
            ]
        );
        // a write through `get_mut` finds its member the same way
        assert_eq!(map.get_mut(&names[1]).copied(), Some(1));
        // enumeration moves nothing
        assert_eq!(keys(&map), keys(&map));
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1035-1046
    #[test]
    fn a_surrogate_pair_and_the_empty_name_resolve_by_their_units() {
        let names = [
            TjsString::from("\u{29e3d}"),
            TjsString::new(),
            TjsString::from("plain"),
        ];
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            assert_eq!(map.insert(name.clone(), index as u32), None);
        }
        for (index, name) in names.iter().enumerate() {
            assert_eq!(map.get(name), Some(&(index as u32)));
        }
        assert_eq!(map.get(&TjsString::from("\u{29e3e}")), None);
        assert_eq!(map.get(&TjsString::from("plain ")), None);
        assert!(map.remove(&names[0]));
        assert!(map.remove(&names[1]));
        assert_eq!(map.get(&names[2]), Some(&2));
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1035-1046
    #[test]
    fn a_bucket_collision_resolves_each_name_and_refuses_the_absent_one() {
        let names = colliding_names(4);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names[..3].iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        for (index, name) in names[..3].iter().enumerate() {
            assert_eq!(map.get(name), Some(&(index as u32)), "{name:?}");
        }
        assert_eq!(map.get(&names[3]), None);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:895-983
    #[test]
    fn clearing_removes_every_member_and_keeps_the_bucket_count() {
        let mut map: MemberMap<u32> = MemberMap::new();
        let names = colliding_names(3);
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        let buckets = map.buckets.len();
        map.clear();
        assert!(map.is_empty());
        assert_eq!(map.iter().count(), 0);
        assert_eq!(map.buckets.len(), buckets);
        assert_eq!(map.insert(names[0].clone(), 7), None);
        assert_eq!(keys(&map), vec![names[0].clone()]);
    }

    /// The fixture's first line, so a regenerated record stays
    /// self-describing.
    const HEADER: &str = "# the engine's answers, recorded by the ignored test `vm::runtime::member::tests::record_member_order`; regenerate with the command in its `#[ignore]` reason";

    /// The names the record's dictionary inserts, in the order it inserts
    /// them.
    const NAMES: [&str; 12] = [
        "key25",
        "k27",
        "v4",
        "e2",
        "key10",
        "at10",
        "gamma",
        "width",
        "key15",
        "key47",
        "member_32",
        "left",
    ];

    /// The names the second probe deletes again.
    const REMOVED: [&str; 2] = ["key10", "width"];

    /// The name the second probe writes back after deleting it.
    const REINSERTED: &str = "key10";

    /// The name the second probe adds after the deletions.
    const ADDED: &str = "at0";

    /// The fixture the record is written to and the check reads.
    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/member_order.txt")
    }

    /// The script that builds one probe's dictionary, and leaves it in the
    /// global `built`.
    ///
    /// The builder is a named function because a script cannot declare one
    /// inside an anonymous body, and it is deleted after the call so that the
    /// next probe starts from the same global object.
    fn script(removals: bool) -> String {
        let mut script =
            String::from("function memberOrder() {\n    var map = new Dictionary();\n");
        for (index, name) in NAMES.iter().enumerate() {
            script.push_str(&format!("    map[\"{name}\"] = {index};\n"));
        }
        if removals {
            for name in REMOVED {
                script.push_str(&format!("    delete map[\"{name}\"];\n"));
            }
            script.push_str(&format!("    map[\"{REINSERTED}\"] = 99;\n"));
            script.push_str(&format!("    map[\"{ADDED}\"] = 100;\n"));
        }
        script
            .push_str("    return map;\n}\nvar built = memberOrder();\ndelete this.memberOrder;\n");
        script
    }

    /// The engine's own enumeration order for one probe.
    ///
    /// The safe wrapper sorts and de-duplicates the names; the ABI entry hands
    /// them back in the order the engine enumerated them, so it is called here
    /// directly.
    #[allow(unsafe_code)]
    fn recorded_order(engine: &Tjs2Engine, probe: &str, removals: bool) -> Vec<String> {
        engine
            .exec_script_discard(&script(removals), probe)
            .expect("the script runs");
        let value = engine
            .eval_retained("built", probe)
            .expect("the dictionary is readable");
        let RetainedValue::Object(dictionary) = value else {
            panic!("probe {probe}: the script did not answer an object");
        };
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: std::ptr::null(),
            array: std::ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: the engine and the dictionary id are live, and `out` is a
        // valid result slot for the call.
        let rc = unsafe {
            crate::tjs2_enum_members(engine.raw(), dictionary.raw_id(), &mut out, &mut error)
        };
        if rc != 0 {
            panic!(
                "probe {probe}: the enumeration failed: {}",
                crate::take_error(error)
            );
        }
        assert_eq!(out.ty, VAL_RETAINED, "probe {probe}: no retained array");
        let array = Tjs2ValueId::from_raw(out.retained as *mut c_void);
        let count = match engine.member(array, "count") {
            Ok(TjsValue::Integer(count)) => count,
            other => panic!("probe {probe}: the array count is not an integer: {other:?}"),
        };
        let mut names = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            match engine.member(array, &index.to_string()) {
                Ok(TjsValue::String(name)) => names.push(name),
                other => panic!("probe {probe}: slot {index} is not a name: {other:?}"),
            }
        }
        // SAFETY: the id came from the call above and has not been released.
        unsafe { crate::tjs2_release_value(engine.raw(), array) };
        engine
            .exec_script_discard("delete this.built;", "member_order_cleanup")
            .expect("the dictionary is dropped");
        names
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1094-1127
    #[test]
    #[ignore = "records the fixture: cargo test -p tjs --lib vm::runtime::member::tests::record_member_order -- --ignored --nocapture"]
    fn record_member_order() {
        let engine = Tjs2Engine::new().expect("the engine starts");
        let mut table = String::from(HEADER);
        table.push('\n');
        for (probe, removals) in [("insert", false), ("remove", true)] {
            let order = recorded_order(&engine, probe, removals);
            table.push_str(&format!("{probe}\t{}\n", order.join(",")));
        }
        let path = fixture_path();
        fs::write(&path, table).expect("the fixture is writable");
        println!("wrote {}", path.display());
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:780-795
    #[test]
    fn a_removed_slot_is_reused_by_a_later_arrival() {
        let names = colliding_names(4);
        let mut map: MemberMap<u32> = MemberMap::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), index as u32);
        }
        assert_eq!(
            keys(&map),
            vec![
                names[3].clone(),
                names[2].clone(),
                names[1].clone(),
                names[0].clone(),
            ]
        );
        assert!(map.remove(&names[2]));
        assert!(map.remove(&names[1]));
        let slots = map.overflow.len();
        assert_eq!(map.free.len(), 2);
        assert_eq!(map.insert(names[1].clone(), 1), None);
        // the freed slot is taken again rather than the arena growing, and the
        // arrival joins the head of the chain
        assert_eq!(map.free.len(), 1);
        assert_eq!(map.overflow.len(), slots);
        assert_eq!(
            keys(&map),
            vec![names[1].clone(), names[3].clone(), names[0].clone()]
        );
    }
}
