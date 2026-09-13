//! Explicit font configuration (`fonts.json`).
//!
//! krkr-rs deliberately performs **no implicit font selection of its own**:
//! the set of usable faces and the fallback order are supplied by the
//! application, not guessed from the system font database. This module is the
//! schema + loader for that configuration; [`crate::font::set_font_config`]
//! installs it process-wide and [`crate::font::resolve_face`] applies it.
//!
//! # Schema
//!
//! ```json
//! {
//!   "faces": {
//!     "MS Gothic": "fonts/msgothic.ttf",
//!     "MS 明朝": { "path": "fonts/msmincho.ttc", "index": 0 }
//!   },
//!   "fallback": [
//!     "fonts/noto-sans-jp.ttf",
//!     { "path": "fonts/noto-cjk.ttc", "index": 1 }
//!   ],
//!   "allow_system_discovery": false
//! }
//! ```
//!
//! * `faces` maps a requested face name to a font file. Names are matched
//!   case-insensitively (see [`crate::font::resolve_face`]).
//! * `fallback` is an ordered list of font files; the first one that loads is
//!   used when the requested face is not mapped (or fails to load).
//! * `allow_system_discovery` opts back into the legacy
//!   [`crate::font::FontFace::discover_system_jp`] system scan. It defaults to
//!   `false`: with a config installed, krkr-rs never touches the system font
//!   database unless this is explicitly set.
//!
//! Both `faces` entries and `fallback` entries accept either a bare string
//! path (collection index `0`) or an object `{ "path": "...", "index": N }`
//! for TTC/OTC collections.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One font source: a bare path string or an indexed collection entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum FontEntry {
    /// A plain path; collection index `0`.
    Path(String),
    /// A path plus a TTC/OTC collection index.
    Indexed {
        /// Font file path.
        path: String,
        /// Face index inside a collection (defaults to `0`).
        #[serde(default)]
        index: u32,
    },
}

impl FontEntry {
    /// The font file path.
    pub fn path(&self) -> &Path {
        Path::new(match self {
            FontEntry::Path(path) => path,
            FontEntry::Indexed { path, .. } => path,
        })
    }

    /// The collection face index (`0` for plain paths).
    pub fn index(&self) -> u32 {
        match self {
            FontEntry::Path(_) => 0,
            FontEntry::Indexed { index, .. } => *index,
        }
    }
}

/// The parsed `fonts.json` configuration.
///
/// See the [module docs](self) for the JSON schema and resolution semantics.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FontConfig {
    /// Requested face name → font file (case-insensitive lookup).
    #[serde(default)]
    pub faces: HashMap<String, FontEntry>,
    /// Ordered fallback chain; the first entry that loads wins.
    #[serde(default)]
    pub fallback: Vec<FontEntry>,
    /// Opt back into the system font database scan when nothing else resolves.
    #[serde(default)]
    pub allow_system_discovery: bool,
}

impl FontConfig {
    /// Parse a config from a JSON string.
    ///
    /// Returns a [`FontConfigError::Parse`] with a clear message on malformed
    /// JSON (unknown face-name keys are fine; the value shape is validated).
    pub fn from_json_str(json: &str) -> Result<Self, FontConfigError> {
        serde_json::from_str(json).map_err(|source| FontConfigError::Parse { path: None, source })
    }

    /// Read and parse `path`.
    ///
    /// Returns a clear error for both a missing/unreadable file and malformed
    /// JSON.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, FontConfigError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| FontConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| FontConfigError::Parse {
            path: Some(path.to_path_buf()),
            source,
        })
    }

    /// Case-insensitive lookup of a face name. Exact key match wins; the map
    /// is then scanned for a key that lowercases to the same string.
    pub(crate) fn face(&self, name: &str) -> Option<&FontEntry> {
        self.faces.get(name).or_else(|| {
            let wanted = name.to_lowercase();
            self.faces
                .iter()
                .find(|(key, _)| key.to_lowercase() == wanted)
                .map(|(_, entry)| entry)
        })
    }
}

/// Errors produced while loading a [`FontConfig`].
#[derive(Debug)]
pub enum FontConfigError {
    /// The file could not be read.
    Io {
        /// The file that was being read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The bytes are not a valid `fonts.json`.
    Parse {
        /// The file that was being parsed, if any.
        path: Option<PathBuf>,
        /// The underlying JSON error (includes line/column).
        source: serde_json::Error,
    },
}

impl fmt::Display for FontConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontConfigError::Io { path, source } => {
                write!(f, "cannot read font config {}: {source}", path.display())
            }
            FontConfigError::Parse {
                path: Some(path),
                source,
            } => write!(f, "malformed font config {}: {source}", path.display()),
            FontConfigError::Parse { path: None, source } => {
                write!(f, "malformed font config: {source}")
            }
        }
    }
}

impl std::error::Error for FontConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FontConfigError::Io { source, .. } => Some(source),
            FontConfigError::Parse { source, .. } => Some(source),
        }
    }
}
