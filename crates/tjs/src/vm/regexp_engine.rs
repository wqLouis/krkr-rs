//! The regular-expression engine the `RegExp` class runs on.
//!
//! A pattern is compiled in the Perl-compatible syntax over the UTF-16 code
//! units a script's text is made of. The engine reads valid UTF-8, so the text
//! is transcoded one character at a time: a surrogate pair becomes the one
//! character it denotes, a lone surrogate becomes the replacement character,
//! and the transcoding keeps the code-unit boundary of every character so that
//! a match's offsets can be read back in code units.
//!
//! A region's offsets are UTF-16LE byte offsets — two bytes per code unit —
//! relative to the start of the searched range, which is the unit the class's
//! state arithmetic is stated in.
//!
//! A search the pattern makes empty is refused when the expression was
//! compiled with [`OPTION_FIND_NOT_EMPTY`]; the search then continues one
//! character further for a match that consumes text.

use fancy_regex::{Captures, Error, ParseError, Regex, RegexBuilder};

/// The option that makes a case-insensitive match.
pub(crate) const OPTION_IGNORECASE: u32 = 1;
/// The option that refuses an empty match.
pub(crate) const OPTION_FIND_NOT_EMPTY: u32 = 32;
/// The option that requests a captured group.
pub(crate) const OPTION_CAPTURE_GROUP: u32 = 256;
/// The highest option bit the compile accepts, as the engine's mask.
pub(crate) const OPTION_MASK: u32 = (OPTION_MAXBIT << 1) - 1;
/// The highest option bit the engine defines.
const OPTION_MAXBIT: u32 = 67_108_864;

/// A compiled expression.
pub(crate) struct Expression {
    /// The engine's compiled pattern.
    regex: Regex,
    /// Whether a search refuses a match that consumes nothing.
    refuses_empty: bool,
}

impl Expression {
    /// Compiles `pattern` with `options`.
    ///
    /// The failure is the message for the pattern, which is the text a
    /// script reads from the raised exception.
    pub(crate) fn compile(pattern: &[u16], options: u32) -> Result<Self, String> {
        let pattern = Text::decode(pattern).text;
        let mut builder = RegexBuilder::new(&pattern);
        builder.case_insensitive(options & OPTION_IGNORECASE != 0);
        builder.oniguruma_mode(true);
        match builder.build() {
            Ok(regex) => Ok(Self {
                regex,
                refuses_empty: options & OPTION_FIND_NOT_EMPTY != 0,
            }),
            Err(error) => Err(refusal(&error)),
        }
    }

    /// Searches `target` from `start` through `end`, both in code units.
    ///
    /// The answer is the region the search filled, or `None` when the
    /// pattern did not match. A region's offsets are relative to `start`.
    pub(crate) fn search(
        &self,
        target: &[u16],
        start: usize,
        end: usize,
    ) -> Result<Option<Region>, RegionError> {
        let window = target.get(start..end).ok_or(RegionError)?;
        let window = Text::decode(window);
        let text = window.text.as_str();
        let mut from = 0usize;
        loop {
            let Some(captures) = self
                .regex
                .captures_from_pos(text, from)
                .map_err(|_| RegionError)?
            else {
                return Ok(None);
            };
            let whole = captures.get(0).ok_or(RegionError)?;
            if !self.refuses_empty || whole.end() > whole.start() {
                return Region::read(&window, &captures).map(Some);
            }
            let Some(character) = text.get(whole.end()..).and_then(|rest| rest.chars().next())
            else {
                return Ok(None);
            };
            from = whole
                .end()
                .checked_add(character.len_utf8())
                .ok_or(RegionError)?;
        }
    }
}

/// The result of a search: the offsets of every captured slot.
pub(crate) struct Region {
    /// The start of each slot, or -1.
    beg: Vec<i32>,
    /// The end of each slot, or -1.
    end: Vec<i32>,
}

impl Region {
    /// Reads a search's captures into the slots of the text they matched.
    fn read(text: &Text, captures: &Captures<'_, str>) -> Result<Self, RegionError> {
        let mut beg = Vec::with_capacity(captures.len());
        let mut end = Vec::with_capacity(captures.len());
        for slot in 0..captures.len() {
            match captures.get(slot) {
                Some(group) => {
                    let start = text.unit_of(group.start()).ok_or(RegionError)?;
                    let finish = text.unit_of(group.end()).ok_or(RegionError)?;
                    beg.push(byte_offset(start)?);
                    end.push(byte_offset(finish)?);
                }
                None => {
                    beg.push(-1);
                    end.push(-1);
                }
            }
        }
        Ok(Self { beg, end })
    }

    /// The number of slots, the whole match included.
    pub(crate) fn len(&self) -> usize {
        self.beg.len()
    }

    /// The byte offset slot `index` starts at.
    pub(crate) fn beg(&self, index: usize) -> i32 {
        self.beg.get(index).copied().unwrap_or(-1)
    }

    /// The byte offset slot `index` ends at.
    pub(crate) fn end(&self, index: usize) -> i32 {
        self.end.get(index).copied().unwrap_or(-1)
    }
}

/// A text the engine can search, with its code-unit boundaries.
struct Text {
    /// The text as UTF-8.
    text: String,
    /// The UTF-8 offset and the code-unit offset of each character
    /// boundary.
    boundaries: Vec<(usize, usize)>,
}

impl Text {
    /// Transcodes `units` into the text the engine reads.
    fn decode(units: &[u16]) -> Self {
        let mut text = String::with_capacity(units.len());
        let mut boundaries = Vec::with_capacity(units.len() + 1);
        let mut unit = 0usize;
        for decoded in char::decode_utf16(units.iter().copied()) {
            let (character, width) = match decoded {
                Ok(character) => (character, character.len_utf16()),
                Err(_lone) => (char::REPLACEMENT_CHARACTER, 1),
            };
            boundaries.push((text.len(), unit));
            text.push(character);
            unit += width;
        }
        boundaries.push((text.len(), unit));
        Self { text, boundaries }
    }

    /// The code-unit offset of the UTF-8 offset `byte`, when it addresses
    /// a character boundary.
    fn unit_of(&self, byte: usize) -> Option<usize> {
        let index = self
            .boundaries
            .binary_search_by_key(&byte, |&(offset, _)| offset)
            .ok()?;
        self.boundaries.get(index).map(|&(_, unit)| unit)
    }
}

/// The byte offset a code-unit offset names.
fn byte_offset(units: usize) -> Result<i32, RegionError> {
    let bytes = units.checked_mul(2).ok_or(RegionError)?;
    i32::try_from(bytes).map_err(|_| RegionError)
}

/// The message a refused pattern reports.
fn refusal(error: &Error) -> String {
    match error {
        Error::ParseError(_, ParseError::UnclosedOpenParen) => {
            String::from("end pattern with unmatched parenthesis")
        }
        _ => error.to_string(),
    }
}

/// A search whose offsets this target cannot hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RegionError;
