//! Photoshop descriptor parser (reference `psdparse/psddesc.{h,cpp}`).
//!
//! Descriptors encode the layer-comp and slice metadata. The reference builds
//! a tree of `DescriptorItem` subclasses with typed downcasts; this port uses
//! one [`Value`] enum and extracts the handful of fields the plugin exposes.

use std::collections::BTreeMap;

use super::parse::{PsdError, Reader, Result};

/// One descriptor value.
///
/// Only the fields the plugin actually reads are stored (see the accessors
/// below); the payload-less variants exist so the parser consumes exactly
/// the bytes the reference `readItem` would — keeping the descriptor cursor
/// aligned without keeping values nobody reads.
#[derive(Debug, Clone)]
pub(crate) enum Value {
    /// An object reference (`obj `); consumed but not interpreted.
    Reference,
    /// A nested descriptor (`Objc` / `GlbO`).
    Descriptor(Descriptor),
    /// A list (`VlLs`).
    List(Vec<Value>),
    /// A double (`doub`); consumed, never read.
    Double,
    /// A unit float (`UntF`); consumed, never read.
    UnitFloat,
    /// A unicode string (`TEXT`).
    Text(String),
    /// An enumerated value (`enum`); consumed, never read.
    Enumerated,
    /// A 32-bit integer (`long`).
    Integer(i32),
    /// A boolean (`bool`).
    Boolean(bool),
    /// A class (`type` / `GlbC`); consumed, never read.
    Class,
    /// An alias string (`alis`); consumed, never read.
    Alias,
}

impl Value {
    /// The integer payload, when this is an [`Value::Integer`].
    pub(crate) fn as_integer(&self) -> Option<i32> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// The nested descriptor, when this is an [`Value::Descriptor`].
    pub(crate) fn as_descriptor(&self) -> Option<&Descriptor> {
        match self {
            Self::Descriptor(descriptor) => Some(descriptor),
            _ => None,
        }
    }
}

/// A parsed descriptor: its keyed items.
///
/// The leading unicode name and class id the reference keeps on its
/// `DescriptorItem` are consumed in [`read_descriptor`] (the cursor must
/// advance past them) but never read by this port, so they are not stored.
#[derive(Debug, Clone, Default)]
pub(crate) struct Descriptor {
    pub(crate) items: BTreeMap<String, Value>,
}

impl Descriptor {
    /// An integer item.
    pub(crate) fn integer(&self, key: &str) -> Option<i32> {
        self.items.get(key).and_then(Value::as_integer)
    }

    /// A boolean item.
    pub(crate) fn boolean(&self, key: &str) -> Option<bool> {
        match self.items.get(key) {
            Some(Value::Boolean(value)) => Some(*value),
            _ => None,
        }
    }

    /// A text item.
    pub(crate) fn text(&self, key: &str) -> Option<&str> {
        match self.items.get(key) {
            Some(Value::Text(value)) => Some(value),
            _ => None,
        }
    }

    /// A list item.
    pub(crate) fn list(&self, key: &str) -> Option<&[Value]> {
        match self.items.get(key) {
            Some(Value::List(values)) => Some(values),
            _ => None,
        }
    }

    /// A nested descriptor item.
    pub(crate) fn descriptor(&self, key: &str) -> Option<&Descriptor> {
        self.items.get(key).and_then(Value::as_descriptor)
    }
}

/// Read a keyed id: a length-prefixed string, where a zero length means the
/// next four bytes are the id (reference `readId`).
fn read_id(r: &mut Reader<'_>) -> Result<String> {
    let mut id_size = r.i32()?;
    if id_size == 0 {
        id_size = 4;
    }
    if id_size < 0 {
        return Err(PsdError::Invalid("negative descriptor id".into()));
    }
    let bytes = r.take(id_size as usize)?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Read one value, dispatching on its 4-byte type code (reference `readItem`).
fn read_item(r: &mut Reader<'_>) -> Result<Value> {
    let type_code = r.key4()?;
    match &type_code {
        b"obj " => read_reference(r),
        b"Objc" | b"GlbO" => Ok(Value::Descriptor(read_descriptor(r)?)),
        b"VlLs" => {
            let count = r.i32()?;
            let mut values = Vec::new();
            for _ in 0..count.max(0) {
                values.push(read_item(r)?);
            }
            Ok(Value::List(values))
        }
        b"doub" => {
            let _ = r.u64()?;
            Ok(Value::Double)
        }
        b"UntF" => {
            let _ = r.u32()?;
            let _ = r.u64()?;
            Ok(Value::UnitFloat)
        }
        b"TEXT" => Ok(Value::Text(r.unicode_string()?)),
        b"enum" => {
            let _ = read_id(r)?;
            let _ = read_id(r)?;
            Ok(Value::Enumerated)
        }
        b"long" => Ok(Value::Integer(r.i32()?)),
        b"bool" => Ok(Value::Boolean(r.u8()? != 0)),
        b"type" | b"GlbC" => {
            let _ = r.unicode_string()?;
            let _ = read_id(r)?;
            Ok(Value::Class)
        }
        b"alis" => {
            let size = r.i32()?;
            if size < 0 {
                return Err(PsdError::Invalid("negative alias size".into()));
            }
            let _ = r.take(size as usize)?;
            Ok(Value::Alias)
        }
        // `tdta` (raw data) carries no length and cannot be skipped, so it
        // aborts the descriptor (the reference does the same).
        _ => Err(PsdError::Invalid(format!(
            "unsupported descriptor type {:?}",
            String::from_utf8_lossy(&type_code)
        ))),
    }
}

/// Consume an object reference (`obj `) without interpreting it.
fn read_reference(r: &mut Reader<'_>) -> Result<Value> {
    let count = r.i32()?;
    for _ in 0..count.max(0) {
        let ref_type = r.key4()?;
        match &ref_type {
            b"prop" => {
                let _name = r.unicode_string()?;
                let _class_id = read_id(r)?;
                let _key_id = read_id(r)?;
            }
            b"Clss" => {
                let _name = r.unicode_string()?;
                let _class_id = read_id(r)?;
            }
            b"Enmr" => {
                let _name = r.unicode_string()?;
                let _class_id = read_id(r)?;
                let _type_id = read_id(r)?;
                let _enum_id = read_id(r)?;
            }
            b"rele" => {
                let _name = r.unicode_string()?;
                let _class_id = read_id(r)?;
                let _offset = r.i32()?;
            }
            b"Idnt" | b"indx" => {
                let _value = r.i32()?;
            }
            b"name" => {
                let _name = r.unicode_string()?;
            }
            _ => {
                return Err(PsdError::Invalid("unsupported reference type".into()));
            }
        }
    }
    Ok(Value::Reference)
}

/// Read a descriptor: consume its unicode name and class id (cursor
/// alignment only — neither is retained, see [`Descriptor`]) and its keyed
/// items.
pub(crate) fn read_descriptor(r: &mut Reader<'_>) -> Result<Descriptor> {
    let _ = r.unicode_string()?;
    let _ = read_id(r)?;
    let count = r.i32()?;
    let mut items = BTreeMap::new();
    for _ in 0..count.max(0) {
        let key = read_id(r)?;
        let value = read_item(r)?;
        items.insert(key, value);
    }
    Ok(Descriptor { items })
}
