//! The random values a script can draw: the small generator behind
//! `Math.random` and the Mersenne Twister a `RandomGenerator` owns.
//!
//! `Math.random` answers a real in `[0, 1)` from a four-word xorshift whose
//! state is the class's. A `RandomGenerator` answers a real, a 32-bit, a
//! 63-bit or a 64-bit integer from a Mersenne Twister, and it can be seeded
//! from an integer, from a state another generator serialised, or from the
//! clock. Its `serialize` member answers the state that reconstructs the
//! sequence.
//!
//! # The two generators
//!
//! The xorshift is a 128-bit state, seeded from one 32-bit word; every draw
//! advances it by one step and answers the fourth word after the xorshift
//! mix. The real divides the word by `2^32`.
//!
//! The Mersenne Twister is the 32-bit Cokus arrangement: 624 state words,
//! a twist every 624 draws, and a tempering step for every answer. A real
//! is built from two 32-bit answers, as the high and low halves of a 52-bit
//! significand, so it has the full double precision and lands in `[0, 1)`.
//! The 63-bit answer takes the sign bit off the same two-word value; the
//! 64-bit answer keeps it.
//!
//! # The seed
//!
//! One integer seed is spread over two 32-bit key words, the low half first.
//! No seed at all uses one 32-bit word — the clock the caller supplies — and
//! a host that has 128 bits of entropy supplies them through the byte form,
//! which folds the bytes into the 32 key words.
//!
//! # The serialised state
//!
//! The state is the 624 words in lowercase hexadecimal, eight digits each, in
//! the order the generator uses them; `left` is the draws left before the
//! next twist and `next` is the word the next answer reads. A state whose
//! `next` is outside the array is refused rather than read past the end.

use std::rc::Rc;

use crate::vm::runtime::dictionary::Dictionary;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant, VariantError};

/// The number of state words the Mersenne Twister uses.
const N: usize = 624;

/// The offset of the second group of words in the twist.
const M: usize = 397;

/// The twist's constant vector.
const MATRIX_A: u32 = 0x9908_b0df;

/// The most significant bit of a word.
const UMASK: u32 = 0x8000_0000;

/// The least significant bits of a word.
const LMASK: u32 = 0x7fff_ffff;

/// The number of hexadecimal digits one state word is written with.
const WORD_DIGITS: usize = 8;

/// The state text's length, in code units.
const STATE_UNITS: usize = N * WORD_DIGITS;

/// The number of bytes a host's entropy source fills.
pub const ENTROPY_BYTES: usize = 32;

/// The fresh seed a generator uses when its seed argument is absent.
///
/// A host that has 128 bits of entropy names them; a host that does not names
/// the clock's low 32 bits instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seed {
    /// One 32-bit word, the clock's low half.
    Time(u32),
    /// The host's 128 bits of entropy.
    Entropy([u8; ENTROPY_BYTES]),
}

/// The xorshift behind `Math.random`.
///
/// The class owns one of these; a draw advances it. The state is the four
/// words the seed spread into.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xorshift {
    /// The four state words.
    seeds: [u32; 4],
}

impl Xorshift {
    /// The generator a 32-bit seed starts.
    pub fn seeded(seed: u32) -> Self {
        let mut seeds = [0u32; 4];
        let mut seed = seed;
        for (index, word) in seeds.iter_mut().enumerate() {
            seed = 1_812_433_253u32
                .wrapping_mul(seed ^ (seed >> 30))
                .wrapping_add(u32::try_from(index).expect("the xorshift has four words"));
            *word = seed;
        }
        Self { seeds }
    }

    /// The next 32-bit answer.
    pub fn next_u32(&mut self) -> u32 {
        let t = self.seeds[0] ^ (self.seeds[0] << 11);
        self.seeds[0] = self.seeds[1];
        self.seeds[1] = self.seeds[2];
        self.seeds[2] = self.seeds[3];
        self.seeds[3] = (self.seeds[3] ^ (self.seeds[3] >> 19)) ^ (t ^ (t >> 8));
        self.seeds[3]
    }

    /// The next answer as a real in `[0, 1)`.
    pub fn next_real(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }
}

/// The state a generator serialises to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedState {
    /// The 624 state words as lowercase hexadecimal, eight digits each.
    pub state: TjsString,
    /// The draws left before the next twist.
    pub left: i64,
    /// The state word the next answer reads.
    pub next: i64,
}

/// A `RandomGenerator` instance's state.
///
/// Every drawing member advances the state; `serialize` reads it without
/// advancing, and `randomize` replaces it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomGenerator {
    /// The Mersenne Twister's words, draws left and next word.
    words: MersenneTwister,
}

impl RandomGenerator {
    /// The generator a constructor's arguments start.
    ///
    /// The arguments are [`RandomGenerator::randomize`]'s: none uses `fresh`,
    /// one integer seeds from it, and one object is a state. Further arguments
    /// are ignored.
    ///
    /// # Errors
    ///
    /// [`RandomError::NotReconstructive`] when the object is not a state, and
    /// [`RandomError::Convert`] when the integer argument has no integer.
    pub fn new(arguments: &[Variant], fresh: Seed) -> Result<Self, RandomError> {
        let mut generator = Self::seeded(0);
        generator.randomize(arguments, fresh)?;
        Ok(generator)
    }

    /// The generator one integer seed starts.
    pub fn seeded(seed: i64) -> Self {
        Self {
            words: MersenneTwister::from_key(&[low_word(seed), high_word(seed)]),
        }
    }

    /// The generator one 32-bit word starts.
    pub fn seeded_from_word(seed: u32) -> Self {
        Self {
            words: MersenneTwister::from_word(seed),
        }
    }

    /// The generator a host's 128 bits of entropy start.
    pub fn seeded_from_entropy(entropy: &[u8; ENTROPY_BYTES]) -> Self {
        Self {
            words: MersenneTwister::from_entropy(entropy),
        }
    }

    /// The generator a serialised state reconstructs.
    ///
    /// # Errors
    ///
    /// [`RandomError::NotReconstructive`] when `next` is not a word of the
    /// state array.
    pub fn restored(state: [u32; N], left: i32, next: i64) -> Result<Self, RandomError> {
        Ok(Self {
            words: MersenneTwister::restored(state, left, next)?,
        })
    }

    /// Replaces the state from a call's arguments.
    ///
    /// No argument uses `fresh`; one integer argument seeds from it; one
    /// object argument is the state `serialize` wrote. Further arguments are
    /// ignored.
    ///
    /// # Errors
    ///
    /// [`RandomError::NotReconstructive`] when the object is not a state,
    /// and [`RandomError::Convert`] when the integer argument has no integer.
    pub fn randomize(&mut self, arguments: &[Variant], fresh: Seed) -> Result<(), RandomError> {
        match arguments.first() {
            None => {
                self.words = match fresh {
                    Seed::Time(word) => MersenneTwister::from_word(word),
                    Seed::Entropy(entropy) => MersenneTwister::from_entropy(&entropy),
                };
                Ok(())
            }
            Some(Variant::Object(closure)) => self.randomize_from_object(closure),
            Some(value) => {
                self.words = MersenneTwister::from_key(&seeded_words(value.to_integer()?));
                Ok(())
            }
        }
    }

    /// The next real in `[0, 1)`.
    ///
    /// # Errors
    ///
    /// [`RandomError::StateRange`] when a malformed state has no word left to
    /// read.
    pub fn random(&mut self) -> Result<f64, RandomError> {
        self.words.rand_double()
    }

    /// The next 32-bit answer.
    ///
    /// # Errors
    ///
    /// [`RandomError::StateRange`] when a malformed state has no word left to
    /// read.
    pub fn random32(&mut self) -> Result<u32, RandomError> {
        self.words.int32()
    }

    /// The next 63-bit answer.
    ///
    /// # Errors
    ///
    /// [`RandomError::StateRange`] when a malformed state has no word left to
    /// read.
    pub fn random63(&mut self) -> Result<i64, RandomError> {
        let value = self.two_words()? & 0x7fff_ffff_ffff_ffff;
        Ok(i64::from_ne_bytes(value.to_ne_bytes()))
    }

    /// The next 64-bit answer.
    ///
    /// # Errors
    ///
    /// [`RandomError::StateRange`] when a malformed state has no word left to
    /// read.
    pub fn random64(&mut self) -> Result<i64, RandomError> {
        let value = self.two_words()?;
        Ok(i64::from_ne_bytes(value.to_ne_bytes()))
    }

    /// The current state, without advancing.
    pub fn serialize(&self) -> SerializedState {
        SerializedState {
            state: self.words.state_text(),
            left: i64::from(self.words.left),
            next: i64::try_from(self.words.next).expect("a state index fits an integer"),
        }
    }

    /// The two-word value a 63- or 64-bit answer is.
    fn two_words(&mut self) -> Result<u64, RandomError> {
        let low = self.words.int32()?;
        let high = self.words.int32()?;
        Ok(u64::from(low) | (u64::from(high) << 32))
    }

    /// Replaces the state from an object argument's members.
    ///
    /// Every failure is the one refusal the member raises: the object is not
    /// a reconstructible state.
    fn randomize_from_object(&mut self, closure: &Closure) -> Result<(), RandomError> {
        let object = closure
            .object
            .as_deref()
            .ok_or(RandomError::NotReconstructive)?;
        let objthis = closure.objthis.as_deref();
        let state = self
            .member(object, objthis, "state")?
            .to_string()
            .map_err(|_| RandomError::NotReconstructive)?;
        let state = parse_state(&state)?;
        let left = self.member(object, objthis, "left")?;
        let left = field32(
            left.to_integer()
                .map_err(|_| RandomError::NotReconstructive)?,
        );
        let next = self.member(object, objthis, "next")?;
        let next = field32(
            next.to_integer()
                .map_err(|_| RandomError::NotReconstructive)?,
        );
        self.words = MersenneTwister::restored(state, left, i64::from(next))?;
        Ok(())
    }

    /// One of the object's required members.
    fn member(
        &self,
        object: &dyn Dispatch,
        objthis: Option<&dyn Dispatch>,
        name: &str,
    ) -> Result<Variant, RandomError> {
        object
            .prop_get(&TjsString::from(name), MemberFlags::MUST_EXIST, objthis)
            .map_err(|_| RandomError::NotReconstructive)
    }
}

/// A member of the `RandomGenerator` class.
///
/// [`RandomGeneratorMember::ALL`] lists the members in the order the class
/// registers them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RandomGeneratorMember {
    /// The empty finaliser.
    Finalize,
    /// Replaces the state from the arguments.
    Randomize,
    /// The next real in `[0, 1)`.
    Random,
    /// The next 32-bit answer.
    Random32,
    /// The next 63-bit answer.
    Random63,
    /// The next 64-bit answer.
    Random64,
    /// The state that reconstructs the sequence.
    Serialize,
}

impl RandomGeneratorMember {
    /// Every member, in the order the class registers them.
    pub const ALL: &'static [RandomGeneratorMember] = &[
        Self::Finalize,
        Self::Randomize,
        Self::Random,
        Self::Random32,
        Self::Random63,
        Self::Random64,
        Self::Serialize,
    ];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        Self::ALL
            .iter()
            .copied()
            .find(|member| units.iter().copied().eq(member.name().encode_utf16()))
    }

    /// The name the class publishes the member under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Finalize => "finalize",
            Self::Randomize => "randomize",
            Self::Random => "random",
            Self::Random32 => "random32",
            Self::Random63 => "random63",
            Self::Random64 => "random64",
            Self::Serialize => "serialize",
        }
    }

    /// The fewest parameters a call accepts.
    pub fn min_params(self) -> usize {
        match self {
            Self::Finalize
            | Self::Randomize
            | Self::Random
            | Self::Random32
            | Self::Random63
            | Self::Random64
            | Self::Serialize => 0,
        }
    }

    /// Calls the member on `generator`.
    ///
    /// `fresh` is the seed [`RandomGeneratorMember::Randomize`] uses when the
    /// call has no argument. Every member ignores parameters beyond the first
    /// it reads.
    ///
    /// # Errors
    ///
    /// [`RandomError::NotReconstructive`] when a seed object is not a state,
    /// [`RandomError::Convert`] when a seed has no integer, and
    /// [`RandomError::StateRange`] when a malformed state has no word left to
    /// read.
    pub fn call(
        self,
        generator: &mut RandomGenerator,
        arguments: &[Variant],
        fresh: Seed,
    ) -> Result<Variant, RandomError> {
        match self {
            Self::Finalize => Ok(Variant::Void),
            Self::Randomize => {
                generator.randomize(arguments, fresh)?;
                Ok(Variant::Void)
            }
            Self::Random => Ok(Variant::Real(generator.random()?)),
            Self::Random32 => Ok(Variant::Integer(i64::from(generator.random32()?))),
            Self::Random63 => Ok(Variant::Integer(generator.random63()?)),
            Self::Random64 => Ok(Variant::Integer(generator.random64()?)),
            Self::Serialize => serialize_value(generator.serialize()),
        }
    }
}

/// The object value a serialised state answers.
fn serialize_value(state: SerializedState) -> Result<Variant, RandomError> {
    let dictionary = Rc::new(Dictionary::new());
    let object: Rc<dyn Dispatch> = dictionary.clone();
    let members = [
        ("state", Variant::String(state.state)),
        ("left", Variant::Integer(state.left)),
        ("next", Variant::Integer(state.next)),
    ];
    for (name, value) in members {
        dictionary.prop_set(
            &TjsString::from(name),
            MemberFlags::ENSURE,
            &value,
            Some(object.as_ref()),
        )?;
    }
    Ok(Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object),
    }))
}

/// The failure of a generator operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RandomError {
    /// The value is not a state the generator can reconstruct.
    #[error("Cannot reconstruct random seeds")]
    NotReconstructive,

    /// The state has no word left to read.
    #[error("the random state is outside the generator's array")]
    StateRange,

    /// A seed has no integer.
    #[error(transparent)]
    Convert(#[from] VariantError),

    /// A member of a state object could not be read.
    #[error(transparent)]
    Member(#[from] ObjectError),
}

/// The Mersenne Twister's words and where the next answer reads.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MersenneTwister {
    /// The 624 state words.
    state: [u32; N],
    /// The draws left before the next twist.
    left: i32,
    /// The word the next answer reads.
    next: usize,
}

impl MersenneTwister {
    /// The generator one 32-bit word seeds.
    fn from_word(seed: u32) -> Self {
        let mut state = [0u32; N];
        state[0] = seed;
        for index in 1..N {
            state[index] = 1_812_433_253u32
                .wrapping_mul(state[index - 1] ^ (state[index - 1] >> 30))
                .wrapping_add(u32::try_from(index).expect("the state has 624 words"));
        }
        Self {
            state,
            left: 1,
            next: 0,
        }
    }

    /// The generator a key array seeds.
    fn from_key<const L: usize>(key: &[u32; L]) -> Self {
        let mut generator = Self::from_word(19_650_218);
        let mut i = 1usize;
        let mut j = 0usize;
        let mut k = if N > L { N } else { L };
        while k > 0 {
            generator.state[i] = (generator.state[i]
                ^ ((generator.state[i - 1] ^ (generator.state[i - 1] >> 30))
                    .wrapping_mul(1_664_525)))
            .wrapping_add(key[j])
            .wrapping_add(u32::try_from(j).expect("a key index fits a word"));
            i += 1;
            j += 1;
            if i >= N {
                generator.state[0] = generator.state[N - 1];
                i = 1;
            }
            if j >= L {
                j = 0;
            }
            k -= 1;
        }
        for _ in 1..N {
            generator.state[i] = (generator.state[i]
                ^ ((generator.state[i - 1] ^ (generator.state[i - 1] >> 30))
                    .wrapping_mul(1_566_083_941)))
            .wrapping_sub(u32::try_from(i).expect("a state index fits a word"));
            i += 1;
            if i >= N {
                generator.state[0] = generator.state[N - 1];
                i = 1;
            }
        }
        generator.state[0] = 0x8000_0000;
        generator.left = 1;
        generator.next = 0;
        generator
    }

    /// The generator a host's 128 bits of entropy seed.
    fn from_entropy(entropy: &[u8; ENTROPY_BYTES]) -> Self {
        let mut key = [0u32; ENTROPY_BYTES];
        for (index, word) in key.iter_mut().enumerate() {
            let byte = u32::from(entropy[index]);
            *word = byte + (byte << 8) + (u32::from(entropy[1]) << 16) + (byte << 24);
        }
        Self::from_key(&key)
    }

    /// The generator a serialised state reconstructs.
    fn restored(state: [u32; N], left: i32, next: i64) -> Result<Self, RandomError> {
        let next = usize::try_from(next).map_err(|_| RandomError::NotReconstructive)?;
        if next >= N {
            return Err(RandomError::NotReconstructive);
        }
        Ok(Self { state, left, next })
    }

    /// The next 32-bit answer.
    fn int32(&mut self) -> Result<u32, RandomError> {
        self.left = self.left.wrapping_sub(1);
        if self.left == 0 {
            self.twist();
        }
        if self.next >= N {
            return Err(RandomError::StateRange);
        }
        let word = self.state[self.next];
        self.next += 1;
        Ok(temper(word))
    }

    /// The next real in `[0, 1)`.
    fn rand_double(&mut self) -> Result<f64, RandomError> {
        let low = self.int32()?;
        let high = self.int32()?;
        let significand = (u64::from(low) | (u64::from(high) << 32)) & 0x000f_ffff_ffff_ffff;
        // the significand is below 2^52, so the conversion is exact
        Ok((significand as f64) / 4_503_599_627_370_496.0)
    }

    /// The state's text, without advancing.
    fn state_text(&self) -> TjsString {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut units = Vec::with_capacity(STATE_UNITS);
        for word in &self.state {
            for shift in (0..WORD_DIGITS).rev() {
                let digit = usize::try_from((word >> (shift * 4)) & 0x0000_000f)
                    .expect("a hexadecimal digit fits a usize");
                units.push(u16::from(HEX[digit]));
            }
        }
        TjsString::from(units)
    }

    /// Twists the whole state.
    fn twist(&mut self) {
        let mut position = 0usize;
        for _ in 0..(N - M) {
            let value = (self.state[position] & UMASK) | (self.state[position + 1] & LMASK);
            self.state[position] =
                self.state[position + M] ^ (value >> 1) ^ twist_constant(self.state[position + 1]);
            position += 1;
        }
        for _ in 0..(M - 1) {
            let value = (self.state[position] & UMASK) | (self.state[position + 1] & LMASK);
            self.state[position] = self.state[position + M - N]
                ^ (value >> 1)
                ^ twist_constant(self.state[position + 1]);
            position += 1;
        }
        let value = (self.state[position] & UMASK) | (self.state[0] & LMASK);
        self.state[position] =
            self.state[position + M - N] ^ (value >> 1) ^ twist_constant(self.state[0]);
        self.left = i32::try_from(N).expect("the state has 624 words");
        self.next = 0;
    }
}

/// The constant a twist adds for an odd word.
fn twist_constant(word: u32) -> u32 {
    if word & 1 != 0 { MATRIX_A } else { 0 }
}

/// The tempering step every answer takes.
fn temper(mut word: u32) -> u32 {
    word ^= word >> 11;
    word ^= (word << 7) & 0x9d2c_5680;
    word ^= (word << 15) & 0xefc6_0000;
    word ^= word >> 18;
    word
}

/// The two 32-bit key words an integer seed spreads over.
fn seeded_words(seed: i64) -> [u32; 2] {
    [low_word(seed), high_word(seed)]
}

/// The low 32 bits of an integer.
fn low_word(seed: i64) -> u32 {
    let bytes = (seed as u64).to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The high 32 bits of an integer.
fn high_word(seed: i64) -> u32 {
    let bytes = (seed as u64).to_le_bytes();
    u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
}

/// An integer field as the 32-bit signed integer the state reads.
fn field32(value: i64) -> i32 {
    let low = value.rem_euclid(4_294_967_296);
    let bits = u32::try_from(low).expect("a remainder below 2^32 fits in a u32");
    i32::from_ne_bytes(bits.to_ne_bytes())
}

/// The state words a `serialize` text names.
fn parse_state(text: &TjsString) -> Result<[u32; N], RandomError> {
    let units = text.as_utf16();
    if units.len() != STATE_UNITS {
        return Err(RandomError::NotReconstructive);
    }
    let mut state = [0u32; N];
    for (index, word) in state.iter_mut().enumerate() {
        let mut value = 0u32;
        for offset in 0..WORD_DIGITS {
            let digit = hex_digit(units[index * WORD_DIGITS + offset])?;
            value = (value << 4) | digit;
        }
        *word = value;
    }
    Ok(state)
}

/// The value of one hexadecimal digit.
fn hex_digit(unit: u16) -> Result<u32, RandomError> {
    match unit {
        0x30..=0x39 => Ok(u32::from(unit - 0x30)),
        0x41..=0x46 => Ok(u32::from(unit - 0x41) + 10),
        0x61..=0x66 => Ok(u32::from(unit - 0x61) + 10),
        _ => Err(RandomError::NotReconstructive),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{RandomError, RandomGenerator, RandomGeneratorMember, Seed, Xorshift, parse_state};
    use crate::vm::runtime::dictionary::Dictionary;
    use crate::vm::runtime::object::{Dispatch, MemberFlags};
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::{Closure, Variant};

    // reference/krkr2/cpp/core/tjs2/tjsMT19937ar-cok.cpp:50-60
    // reference/krkr2/cpp/core/tjs2/tjsMT19937ar-cok.cpp:236-252
    #[test]
    fn the_single_word_seed_is_the_twister_initialisation() {
        let mut generator = RandomGenerator::seeded_from_word(5489);
        assert_eq!(generator.random32().expect("a word"), 3_499_211_612);
        assert_eq!(generator.random32().expect("a word"), 581_869_302);
        assert_eq!(generator.random32().expect("a word"), 3_890_346_734);
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:248-260
    #[test]
    fn the_constructor_uses_the_same_seed_rule() {
        let mut generator =
            RandomGenerator::new(&[], Seed::Time(0)).expect("the constructor seeds");
        assert_eq!(generator.random32().expect("a word"), 2_357_136_044);
        let mut seeded =
            RandomGenerator::new(&[Variant::Integer(1)], Seed::Time(0)).expect("the seed is read");
        assert_eq!(seeded.random32().expect("a word"), 577_090_037);
    }

    // reference/krkr2/cpp/core/tjs2/tjsMT19937ar-cok.cpp:66-101
    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:115-122
    #[test]
    fn one_integer_seed_is_two_key_words() {
        let mut generator = RandomGenerator::seeded(1);
        assert_eq!(generator.random32().expect("a word"), 577_090_037);
        assert_eq!(generator.random32().expect("a word"), 2_444_712_010);
        assert_eq!(generator.random32().expect("a word"), 3_639_700_191);
        let mut negative = RandomGenerator::seeded(-1);
        assert_eq!(negative.random32().expect("a word"), 93_740_670);
        let mut above = RandomGenerator::seeded(4_294_967_296);
        assert_eq!(above.random32().expect("a word"), 485_306_839);
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:205-237
    // reference/krkr2/cpp/core/tjs2/tjsMT19937ar-cok.cpp:282-304
    #[test]
    fn a_real_is_two_words_and_the_integer_forms_share_them() {
        let mut real = RandomGenerator::seeded(1);
        assert_eq!(real.random().expect("a real"), 0.459_055_075_039_143_33);
        let mut wide = RandomGenerator::seeded(1);
        assert_eq!(
            wide.random64().expect("a wide integer"),
            -7_946_785_942_044_036_619
        );
        let mut narrow = RandomGenerator::seeded(1);
        assert_eq!(
            narrow.random63().expect("a narrow integer"),
            1_276_586_094_810_739_189
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:48-65
    #[test]
    fn the_entropy_bytes_fold_into_the_key_words() {
        let mut entropy = [0u8; super::ENTROPY_BYTES];
        for (index, byte) in entropy.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("the entropy source has 32 bytes");
        }
        let mut generator = RandomGenerator::seeded_from_entropy(&entropy);
        assert_eq!(generator.random32().expect("a word"), 1_314_747_608);
        assert_eq!(generator.random32().expect("a word"), 2_561_966_505);
    }

    // reference/krkr2/cpp/core/tjs2/tjsMath.cpp:48-77
    #[test]
    fn the_math_generator_is_the_four_word_xorshift() {
        let mut generator = Xorshift::seeded(1);
        assert_eq!(generator.next_u32(), 128_212_549);
        assert_eq!(generator.next_u32(), 1_125_693_140);
        assert_eq!(generator.next_u32(), 2_849_326_239);
        let mut real = Xorshift::seeded(1);
        assert_eq!(real.next_real(), 0.029_851_810_308_173_3);
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:144-193
    #[test]
    fn a_serialised_state_reconstructs_the_same_sequence() {
        let mut generator = RandomGenerator::seeded(7);
        generator.random32().expect("a word");
        generator.random32().expect("a word");
        let state = generator.serialize();
        assert_eq!(state.state.as_utf16().len(), super::STATE_UNITS);
        assert_eq!(state.left, 623);
        assert_eq!(state.next, 2);
        let words = parse_state(&state.state).expect("the state text parses");
        let left = i32::try_from(state.left).expect("the state has a small left count");
        let mut restored = RandomGenerator::restored(words, left, state.next)
            .expect("the state is reconstructible");
        assert_eq!(restored.random32().expect("a word"), 1_375_296_642);
        assert_eq!(restored.random32().expect("a word"), 2_547_429_643);
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:44-141
    #[test]
    fn an_object_seed_must_carry_the_three_members() {
        let generator = RandomGenerator::seeded(1);
        let state = generator.serialize();
        let dictionary = Rc::new(Dictionary::new());
        let value = |name: &str, value: Variant| {
            dictionary
                .prop_set(&TjsString::from(name), MemberFlags::ENSURE, &value, None)
                .expect("the dictionary takes a member");
        };
        value("state", Variant::String(state.state));
        value("left", Variant::Integer(state.left));
        value("next", Variant::Integer(state.next));
        let object = Variant::Object(Closure {
            object: Some(dictionary.clone()),
            objthis: Some(dictionary),
        });
        let mut restored = RandomGenerator::seeded(2);
        restored
            .randomize(&[object], Seed::Time(0))
            .expect("the state is reconstructible");
        assert_eq!(restored.random32().expect("a word"), 577_090_037);
        let mut broken = RandomGenerator::seeded(2);
        assert_eq!(
            broken.randomize(&[Variant::Object(Closure::null())], Seed::Time(0)),
            Err(RandomError::NotReconstructive)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:262-352
    #[test]
    fn every_member_has_its_name_and_calls_the_generator() {
        use RandomGeneratorMember as Member;
        assert_eq!(Member::ALL.len(), 7);
        for member in Member::ALL {
            assert_eq!(
                Member::lookup(&TjsString::from(member.name())),
                Some(*member)
            );
        }
        let mut generator = RandomGenerator::seeded(1);
        assert!(matches!(
            Member::Random.call(&mut generator, &[], Seed::Time(0)),
            Ok(Variant::Real(_))
        ));
        assert!(matches!(
            Member::Random32.call(&mut generator, &[Variant::Integer(9)], Seed::Time(0)),
            Ok(Variant::Integer(_))
        ));
        assert!(matches!(
            Member::Finalize.call(&mut generator, &[], Seed::Time(0)),
            Ok(Variant::Void)
        ));
    }
}
