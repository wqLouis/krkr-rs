//! Photoshop descriptor reading.
//!
//! Descriptors carry the layer-comp and slice metadata. They are a tagged tree:
//! a unicode name, a class id, then keyed items whose type codes decide the
//! payload that follows. One [`Value`] enum covers every shape and the handful
//! of fields the plugin surface reads are extracted through its accessors.

use std::collections::BTreeMap;

use crate::error::{PsdError, PsdResult};
use crate::parse::Reader;

/// One descriptor value.
///
/// Only the fields the plugin reads are kept (see the accessors below); the
/// payload-less variants exist so the reader consumes exactly the bytes their
/// type codes announce, keeping the descriptor cursor aligned without storing
/// values nobody reads.
#[derive(Debug, Clone)]
pub(crate) enum Value {
    /// An object pointer record (`obj `); consumed but not interpreted.
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
            Self::Reference
            | Self::Descriptor(_)
            | Self::List(_)
            | Self::Double
            | Self::UnitFloat
            | Self::Text(_)
            | Self::Enumerated
            | Self::Boolean(_)
            | Self::Class
            | Self::Alias => None,
        }
    }

    /// The nested descriptor, when this is an [`Value::Descriptor`].
    pub(crate) fn as_descriptor(&self) -> Option<&Descriptor> {
        match self {
            Self::Descriptor(descriptor) => Some(descriptor),
            Self::Reference
            | Self::List(_)
            | Self::Double
            | Self::UnitFloat
            | Self::Text(_)
            | Self::Enumerated
            | Self::Integer(_)
            | Self::Boolean(_)
            | Self::Class
            | Self::Alias => None,
        }
    }
}

/// A parsed descriptor: its keyed items.
///
/// The leading unicode name and class id are consumed by
/// [`read_descriptor`] (the cursor must advance past them) but are not
/// retained because no accessor reads them.
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
/// next four bytes are the id.
fn read_id(r: &mut Reader<'_>) -> PsdResult<String> {
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

/// Read one value, dispatching on its 4-byte type code.
fn read_item(r: &mut Reader<'_>) -> PsdResult<Value> {
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
        // aborts the descriptor.
        _ => Err(PsdError::Invalid(format!(
            "unsupported descriptor type {:?}",
            String::from_utf8_lossy(&type_code)
        ))),
    }
}

/// Consume an object pointer record (`obj `) without interpreting it.
fn read_reference(r: &mut Reader<'_>) -> PsdResult<Value> {
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

/// Read a descriptor: consume its unicode name and class id (cursor alignment
/// only, neither is retained) and then its keyed items.
pub(crate) fn read_descriptor(r: &mut Reader<'_>) -> PsdResult<Descriptor> {
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
