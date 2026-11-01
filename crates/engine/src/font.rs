//! The font registry: the faces an engine registered, the names they answer
//! to, and the default face the per-character fallback uses.
//!
//! One registry holds every face the engine knows: the faces a script
//! resolved, the faces `System.addFont` registered and the face the
//! composition root installed as the engine's default. The registry keeps
//! the registration order, because the order decides two script-visible
//! things:
//!
//! * the default face is the oldest registered entry, and
//! * the registered-name list is newest first and leaves the oldest name out.
//!
//! The names a face answers to are the family names its name table carries,
//! so a face is found under each of them. A name that only a caller knows
//! (the string a script asked for) is an *alias*: it resolves the face but
//! stays out of the registered-name list.

use render::Font;

use crate::hasher::{Map, map};

/// A registered font face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct FontId(usize);

impl FontId {
    /// Returns the registration index of this face.
    pub const fn index(self) -> usize {
        self.0
    }

    /// A key for an in-memory cache test.
    #[cfg(test)]
    pub(crate) const fn test_key(index: usize) -> Self {
        Self(index)
    }
}

/// One registered face: the L1 rasterizer face, the family name the shaping
/// database selects it under and the names it answers to.
#[derive(Debug)]
#[non_exhaustive]
pub struct FontFace {
    /// The names the face answers to, in registration order.
    pub names: Vec<String>,
    /// The family name the shaping database selects the face under.
    pub family: String,
    /// The L1 rasterizer face.
    pub render: Font,
}

/// The registry of the faces an engine knows.
///
/// The registry is the single source of truth for "which faces exist and what
/// are they called". It owns no shaping state and no cache: the shaping
/// database and the per-character caches belong to the text engine that owns
/// the registry.
#[derive(Debug)]
pub struct FontRegistry {
    /// The registered faces, indexed by [`FontId`].
    faces: Vec<FontFace>,
    /// Every known name → the face it resolves, in lower case.
    by_name: Map<String, FontId>,
    /// The registered names, newest first.
    ///
    /// A name that is registered again moves to the front, so the last entry
    /// is the oldest name and the face it resolves is the default face.
    order: Vec<String>,
}

impl Default for FontRegistry {
    fn default() -> Self {
        Self {
            faces: Vec::new(),
            by_name: map(),
            order: Vec::new(),
        }
    }
}

impl FontRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a face and returns its id.
    ///
    /// The face's names resolve it from now on; the first name that was
    /// registered is the oldest name, so the face it resolves is the default
    /// face until another name is registered.
    pub fn register(&mut self, face: FontFace) -> FontId {
        let id = FontId(self.faces.len());
        for name in &face.names {
            self.insert_name(name, id);
        }
        self.faces.push(face);
        id
    }

    /// Resolves `name` to a face, without adding it to the registered-name
    /// list.
    ///
    /// An alias is the name a caller asked for (a face name with a
    /// comma-separated candidate list, a storage path): it must resolve the
    /// same face on the next lookup, but it is not a name the face itself
    /// carries.
    pub fn alias(&mut self, name: &str, id: FontId) {
        self.by_name.insert(name.to_lowercase(), id);
    }

    /// Returns the face registered under `name` (case-insensitive).
    pub fn by_name(&self, name: &str) -> Option<FontId> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    /// Returns the registered face `id`, if it is registered.
    pub fn face(&self, id: FontId) -> Option<&FontFace> {
        self.faces.get(id.index())
    }

    /// The registered names, newest first.
    pub fn names(&self) -> &[String] {
        &self.order
    }

    /// The registered names a font list reports: newest first, without the
    /// oldest name.
    ///
    /// The oldest name is the one the default face is registered under, so a
    /// registry that holds a single name reports an empty list.
    pub fn list_names(&self) -> Vec<String> {
        let mut names = self.order.clone();
        names.pop();
        names
    }

    /// The oldest registered name, the name the default face is registered
    /// under.
    pub fn default_name(&self) -> Option<&str> {
        self.order.last().map(String::as_str)
    }

    /// The default face: the face registered under the oldest name.
    pub fn default_face(&self) -> Option<FontId> {
        self.default_name().and_then(|name| self.by_name(name))
    }

    /// The number of registered faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Returns `true` when no face is registered.
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Records `name` as resolving to `id` and moves it to the front of the
    /// registered-name list.
    fn insert_name(&mut self, name: &str, id: FontId) {
        let key = name.to_lowercase();
        self.order.retain(|entry| entry.to_lowercase() != key);
        self.by_name.insert(key, id);
        self.order.insert(0, name.to_string());
    }
}
