//! The values an engine keeps alive, and the ids that name them.
//!
//! A script value outlives the call that made it only while something holds
//! it: a native keeps an object for the frames that follow, a callback is kept
//! for a timer, a class instance is held while its window is open. The engine
//! therefore keeps the values it is asked to keep and hands each one an id, and
//! a later call names the value by that id instead of carrying a handle of its
//! own.
//!
//! # The id
//!
//! An id is a slot index and a generation. A held slot carries the value the id
//! names; releasing it moves the slot's generation on, so the id never matches
//! again — not while the slot is free, and not after a later retention takes the
//! same slot. A stale id is therefore a typed refusal ([`RetainError::Stale`])
//! and never a read of whatever the slot holds now.
//!
//! The pair is not an address and is never derived from one: it names a
//! position in one table, and the same pair made by another table names that
//! other table's value. An id therefore belongs to the table that made it, and
//! the caller's own handle is what ties an id to its engine — this module cannot
//! tell two tables' ids apart, and does not pretend to. The id has no null form
//! either: the empty case is `Option<RetainedId>`, so no id exists that is
//! "valid" and names nothing.
//!
//! # Why a slot and a generation
//!
//! The engine's count of held values is a leak diagnostic: it answers how many
//! values are held right now. A table whose keys were never reused would keep
//! every release as an absent entry and grow with the session, and the count
//! would stop meaning "held". The generation pair keeps the table the size of
//! the held set: a released slot is handed out again, and the generation is
//! what hides the reuse from an id that is still in someone's hand.
//!
//! # The three shapes a retention takes
//!
//! [`RetainedTable::retain`] keeps a value, [`RetainedTable::retain_object`]
//! keeps an object as the value a call names, and
//! [`RetainedTable::duplicate`] makes a second id for a value that is already
//! held, so a call that consumes a retention does not consume the caller's.
//! Each answers the same id type, and the value is released once per id.

use std::fmt;
use std::rc::Rc;

use super::object::Dispatch;
use super::variant::{Closure, Variant};

/// The id of a value the engine keeps.
///
/// An id names one held value for as long as the value is held; see the module
/// documentation for the release contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RetainedId {
    /// The slot the value is in.
    slot: usize,
    /// The generation the slot was at when the id was made.
    generation: u32,
}

impl fmt::Display for RetainedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.slot, self.generation)
    }
}

/// The reason the table refused an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RetainError {
    /// The id names no value the table holds: it was released, or its slot has
    /// been handed out again since.
    #[error("the retained value {0} is no longer held")]
    Stale(RetainedId),

    /// The id names a value that is not an object.
    #[error("the retained value {0} is not an object")]
    NotAnObject(RetainedId),

    /// The id names the null object.
    #[error("the retained value {0} is the null object")]
    NullObject(RetainedId),
}

/// One slot of the table.
#[derive(Default)]
struct Slot {
    /// The generation the slot is at.
    ///
    /// An id matches the slot only at this generation, and a release moves it
    /// on, which is what makes a released id name nothing.
    generation: u32,
    /// The value the slot holds, or `None` while the slot is free.
    value: Option<Variant>,
}

/// The values one engine keeps alive, with the ids that name them.
#[derive(Default)]
pub struct RetainedTable {
    /// Every slot the table has made.
    slots: Vec<Slot>,
    /// The slots that hold no value, ready to be handed out again.
    ///
    /// A slot whose generation has counted out is not put here: its next
    /// generation cannot be told apart from the one an older id still holds.
    free: Vec<usize>,
    /// How many slots hold a value.
    held: usize,
}

impl RetainedTable {
    /// A table holding no value.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps `value` and answers the id that names it.
    pub fn retain(&mut self, value: Variant) -> RetainedId {
        if let Some(slot) = self.free.pop() {
            // the free list holds only slots this table made, so the index is
            // in range
            let entry = &mut self.slots[slot];
            entry.value = Some(value);
            self.held += 1;
            return RetainedId {
                slot,
                generation: entry.generation,
            };
        }
        let slot = self.slots.len();
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        self.held += 1;
        RetainedId { slot, generation: 0 }
    }

    /// Keeps `object` as an object value, with itself as its receiver.
    ///
    /// This is the shape a handle that holds an object without a value around
    /// it takes: the object answers its own members and is its own receiver,
    /// which is what a construction and a call need.
    pub fn retain_object(&mut self, object: &Rc<dyn Dispatch>) -> RetainedId {
        self.retain(Variant::Object(Closure {
            object: Some(Rc::clone(object)),
            objthis: Some(Rc::clone(object)),
        }))
    }

    /// Makes a second id for a value that is already held.
    ///
    /// Both ids stay live and each releases its own holding, so a call that
    /// consumes the retention it is handed does not consume the caller's.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn duplicate(&mut self, id: RetainedId) -> Result<RetainedId, RetainError> {
        let value = self.value(id)?.clone();
        Ok(self.retain(value))
    }

    /// The value `id` names.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn value(&self, id: RetainedId) -> Result<&Variant, RetainError> {
        let slot = self.slots.get(id.slot).ok_or(RetainError::Stale(id))?;
        if slot.generation != id.generation {
            return Err(RetainError::Stale(id));
        }
        slot.value.as_ref().ok_or(RetainError::Stale(id))
    }

    /// Releases the holding `id` names and answers the value that was released.
    ///
    /// A second release of the same id is [`RetainError::Stale`]: the caller's
    /// own handle is what makes a release happen once, and the table reports a
    /// release it did not perform rather than accepting it.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn release(&mut self, id: RetainedId) -> Result<Variant, RetainError> {
        let slot = self
            .slots
            .get_mut(id.slot)
            .ok_or(RetainError::Stale(id))?;
        if slot.generation != id.generation {
            return Err(RetainError::Stale(id));
        }
        let Some(value) = slot.value.take() else {
            return Err(RetainError::Stale(id));
        };
        // the generation counts out: the slot is never handed out again, so no
        // id can be told apart from an older one that named it
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(id.slot);
        }
        self.held -= 1;
        Ok(value)
    }

    /// How many values the table holds right now.
    pub fn count(&self) -> usize {
        self.held
    }
}

impl fmt::Debug for RetainedTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedTable")
            .field("held", &self.held)
            .field("slots", &self.slots.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{RetainError, RetainedTable};
    use crate::vm::runtime::variant::Variant;

    #[test]
    fn a_held_value_answers_until_it_is_released() {
        let mut table = RetainedTable::new();
        let id = table.retain(Variant::from(1i64));
        assert_eq!(table.count(), 1);
        assert!(matches!(table.value(id), Ok(Variant::Integer(1))));
        assert!(matches!(table.release(id), Ok(Variant::Integer(1))));
        assert_eq!(table.count(), 0);
        assert_eq!(table.value(id).err(), Some(RetainError::Stale(id)));
    }

    #[test]
    fn a_released_slot_handed_out_again_does_not_answer_the_old_id() {
        let mut table = RetainedTable::new();
        let first = table.retain(Variant::from(1i64));
        table.release(first).expect("the value is held");
        let second = table.retain(Variant::from(2i64));
        assert_eq!(first.slot, second.slot, "the slot is reused");
        assert_ne!(first, second, "the generation tells the ids apart");
        assert_eq!(table.value(first).err(), Some(RetainError::Stale(first)));
        assert!(matches!(table.value(second), Ok(Variant::Integer(2))));
    }

    #[test]
    fn a_duplicate_outlives_the_release_of_the_id_it_was_made_from() {
        let mut table = RetainedTable::new();
        let id = table.retain(Variant::from(41i64));
        let copy = table.duplicate(id).expect("the value is held");
        table.release(id).expect("the value is held");
        assert!(matches!(table.value(copy), Ok(Variant::Integer(41))));
        assert_eq!(table.count(), 1);
    }
}
