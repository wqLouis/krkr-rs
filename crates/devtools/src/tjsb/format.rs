//! The bytecode container: the constant pool and the code objects.
//!
//! The layout is:
//!
//! ```text
//! "TJS2"            the file's magic
//! "100\0"           the version
//! u32               the file's own size
//! "DATA"            the constant pool's tag
//! u32               the pool's size, counting this tag and this size field
//! ...               the constant pool
//! "OBJS"            the object section's tag
//! u32               the object section's size
//! i32               the top-level object's index
//! u32               the object's count
//! ...               the objects
//! ```
//!
//! The pool holds, in this order, the byte, short, integer, long-integer and
//! real arrays, then the strings and then the octets. Every array starts with
//! its own count; the byte array is padded to a four-byte boundary, the
//! short array to a two-byte boundary, every string to a two-byte boundary
//! and every octet to a four-byte boundary.
//!
//! Every object starts with the `"TJS2"` tag and its own size, and holds the
//! twelve context fields, the source positions, the code, the constant slots,
//! the super-class getter positions and the member list.

use std::fmt;

/// A malformed bytecode file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytecodeError {
    /// The file does not start with the container's magic.
    BadMagic([u8; 4]),
    /// The file's version tag is not the one the loader accepts.
    BadVersion([u8; 4]),
    /// The file's declared size does not match its length.
    SizeMismatch {
        /// The size the file declares.
        declared: u32,
        /// The size the file actually has.
        actual: u64,
    },
    /// A section tag is not the one the layout requires.
    BadTag {
        /// The tag the layout requires.
        expected: &'static str,
        /// The tag the file holds.
        found: [u8; 4],
    },
    /// The file ends before a value the layout requires.
    Truncated {
        /// What the reader was reading.
        what: &'static str,
    },
    /// A count or a length in the file cannot be used.
    Invalid(&'static str),
}

impl fmt::Display for BytecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(found) => {
                write!(
                    formatter,
                    "not a bytecode file: the magic is {}",
                    describe(found)
                )
            }
            Self::BadVersion(found) => {
                write!(
                    formatter,
                    "not a bytecode file: the version tag is {}",
                    describe(found)
                )
            }
            Self::SizeMismatch { declared, actual } => write!(
                formatter,
                "the file declares {declared} bytes and holds {actual} bytes"
            ),
            Self::BadTag { expected, found } => write!(
                formatter,
                "the section tag is {} where {expected} was expected",
                describe(found)
            ),
            Self::Truncated { what } => write!(formatter, "the file ends before {what}"),
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for BytecodeError {}

/// The bytes as hexadecimal text.
fn describe(bytes: &[u8; 4]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

/// The container's constant pool.
#[derive(Debug, Clone, Default)]
pub struct Constants {
    /// The byte constants.
    pub bytes: Vec<i8>,
    /// The short constants.
    pub shorts: Vec<i16>,
    /// The integer constants.
    pub longs: Vec<i32>,
    /// The long-integer constants.
    pub longlongs: Vec<i64>,
    /// The real constants.
    pub doubles: Vec<f64>,
    /// The string constants.
    pub strings: Vec<String>,
    /// The octet constants.
    pub octets: Vec<Vec<u8>>,
}

/// One code object.
#[derive(Debug, Clone)]
pub struct Object {
    /// The index of the object's parent, or `-1`.
    pub parent: i32,
    /// The index of the object's name in the string table.
    pub name: i32,
    /// The object's context kind.
    pub context_type: i32,
    /// The number of variables the object declares.
    pub max_variable_count: i32,
    /// The number of variables the object keeps in reserve.
    pub variable_reserve_count: i32,
    /// The number of stack frames the object uses.
    pub max_frame_count: i32,
    /// The number of arguments the object declares.
    pub argument_count: i32,
    /// The base of the object's unnamed arguments.
    pub unnamed_argument_base: i32,
    /// The base the object's arguments collapse to.
    pub collapse_base: i32,
    /// The index of the object's property setter, or `-1`.
    pub property_setter: i32,
    /// The index of the object's property getter, or `-1`.
    pub property_getter: i32,
    /// The index of the object's super-class getter, or `-1`.
    pub super_class_getter: i32,
    /// The object's source positions, as code position and source position.
    pub source_positions: Vec<(u32, u32)>,
    /// The object's code.
    pub code: Vec<i16>,
    /// The object's constant slots, as a kind and an index.
    pub data: Vec<(i16, i16)>,
    /// The source positions of the object's super-class getters.
    pub super_class_getter_positions: Vec<u32>,
    /// The object's members, as a name index and an object index.
    pub properties: Vec<(i32, i32)>,
    /// The size the object declares.
    pub declared_size: u32,
}

/// A parsed bytecode file.
#[derive(Debug, Clone)]
pub struct BytecodeFile {
    /// The index of the top-level object, or `-1`.
    pub toplevel: i32,
    /// The file's constant pool.
    pub constants: Constants,
    /// The file's objects.
    pub objects: Vec<Object>,
}

/// The reader over the file's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// The reader's current position.
    fn position(&self) -> usize {
        self.position
    }

    /// The number of bytes left in the file.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    /// Take the next `count` bytes.
    fn take(&mut self, count: usize, what: &'static str) -> Result<&'a [u8], BytecodeError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(BytecodeError::Invalid(
                "a section length overflows the address space",
            ))?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(BytecodeError::Truncated { what })?;
        self.position = end;
        Ok(slice)
    }

    /// Skip the next `count` bytes.
    fn skip(&mut self, count: usize, what: &'static str) -> Result<(), BytecodeError> {
        self.take(count, what).map(|_| ())
    }

    /// The next byte.
    fn u8(&mut self) -> Result<u8, BytecodeError> {
        Ok(self.take(1, "a byte")?[0])
    }

    /// The next signed byte.
    fn i8(&mut self) -> Result<i8, BytecodeError> {
        Ok(i8::from_le_bytes([self.u8()?]))
    }

    /// The next unsigned 16-bit value.
    fn u16(&mut self) -> Result<u16, BytecodeError> {
        let bytes = self.take(2, "a 16-bit value")?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// The next signed 16-bit value.
    fn i16(&mut self) -> Result<i16, BytecodeError> {
        let bytes = self.take(2, "a signed 16-bit value")?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// The next unsigned 32-bit value.
    fn u32(&mut self) -> Result<u32, BytecodeError> {
        let bytes = self.take(4, "a 32-bit value")?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// The next signed 32-bit value.
    fn i32(&mut self) -> Result<i32, BytecodeError> {
        let bytes = self.take(4, "a signed 32-bit value")?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// The next signed 64-bit value.
    fn i64(&mut self) -> Result<i64, BytecodeError> {
        let bytes = self.take(8, "a 64-bit value")?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(array))
    }

    /// The next real value.
    fn f64(&mut self) -> Result<f64, BytecodeError> {
        let bytes = self.take(8, "a real value")?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(f64::from_le_bytes(array))
    }
}

/// A count as a length.
fn count_of(count: u32) -> Result<usize, BytecodeError> {
    usize::try_from(count).map_err(|_| BytecodeError::Invalid("a count does not fit a length"))
}

/// A count the file's remaining bytes can hold.
///
/// Every counted element takes at least one byte, so a count that exceeds
/// the remaining bytes names a truncated file. The check also keeps the
/// allocation the count asks for inside the file's own size.
fn counted(reader: &Reader<'_>, count: u32, what: &'static str) -> Result<usize, BytecodeError> {
    let count = count_of(count)?;
    if count > reader.remaining() {
        return Err(BytecodeError::Truncated { what });
    }
    Ok(count)
}

/// Parse a bytecode file.
pub fn parse(bytes: &[u8]) -> Result<BytecodeFile, BytecodeError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.take(4, "the file's magic")?;
    if magic != b"TJS2" {
        return Err(BytecodeError::BadMagic([
            magic[0], magic[1], magic[2], magic[3],
        ]));
    }
    let version = reader.take(4, "the file's version")?;
    if version != b"100\0" {
        return Err(BytecodeError::BadVersion([
            version[0], version[1], version[2], version[3],
        ]));
    }
    let declared = reader.u32()?;
    let actual = u64::try_from(bytes.len())
        .map_err(|_| BytecodeError::Invalid("the file is too large to address"))?;
    if u64::from(declared) != actual {
        return Err(BytecodeError::SizeMismatch { declared, actual });
    }

    let tag = reader.take(4, "the constant pool's tag")?;
    if tag != b"DATA" {
        return Err(BytecodeError::BadTag {
            expected: "DATA",
            found: [tag[0], tag[1], tag[2], tag[3]],
        });
    }
    let size = reader.u32()?;
    let pool_size = counted(&reader, size, "the constant pool")?;
    if pool_size < 8 {
        return Err(BytecodeError::Invalid(
            "the constant pool's size is smaller than its header",
        ));
    }
    let pool_start = reader.position();
    let constants = read_constants(&mut reader)?;
    let pool_end = reader.position();
    if pool_end - pool_start != pool_size - 8 {
        return Err(BytecodeError::Invalid(
            "the constant pool's contents do not match its size",
        ));
    }

    let tag = reader.take(4, "the object section's tag")?;
    if tag != b"OBJS" {
        return Err(BytecodeError::BadTag {
            expected: "OBJS",
            found: [tag[0], tag[1], tag[2], tag[3]],
        });
    }
    let _section_size = reader.u32()?;
    let toplevel = reader.i32()?;
    let size = reader.u32()?;
    let object_count = counted(&reader, size, "the objects")?;
    let mut objects = Vec::new();
    for _ in 0..object_count {
        objects.push(read_object(&mut reader)?);
    }
    Ok(BytecodeFile {
        toplevel,
        constants,
        objects,
    })
}

/// Read the constant pool.
fn read_constants(reader: &mut Reader<'_>) -> Result<Constants, BytecodeError> {
    let mut constants = Constants::default();

    let size = reader.u32()?;
    let count = counted(reader, size, "the byte array")?;
    for _ in 0..count {
        constants.bytes.push(reader.i8()?);
    }
    let padded = count.next_multiple_of(4);
    reader.skip(padded - count, "the byte array's padding")?;

    let size = reader.u32()?;
    let count = counted(reader, size, "the short array")?;
    for _ in 0..count {
        constants.shorts.push(reader.i16()?);
    }
    let padded = count.next_multiple_of(2);
    reader.skip(padded - count, "the short array's padding")?;

    let size = reader.u32()?;
    let count = counted(reader, size, "the integer array")?;
    for _ in 0..count {
        constants.longs.push(reader.i32()?);
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the long-integer array")?;
    for _ in 0..count {
        constants.longlongs.push(reader.i64()?);
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the real array")?;
    for _ in 0..count {
        constants.doubles.push(reader.f64()?);
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the string array")?;
    for _ in 0..count {
        let length = reader.u32()?;
        let length = usize::try_from(length)
            .map_err(|_| BytecodeError::Invalid("a string's length does not fit a length"))?;
        if length > reader.remaining() / 2 {
            return Err(BytecodeError::Truncated {
                what: "a string's characters",
            });
        }
        let mut units = Vec::with_capacity(length);
        for _ in 0..length {
            units.push(reader.u16()?);
        }
        let padded = length.next_multiple_of(2);
        reader.skip(padded - length, "a string's padding")?;
        constants.strings.push(String::from_utf16_lossy(&units));
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the octet array")?;
    for _ in 0..count {
        let length = reader.u32()?;
        let length = usize::try_from(length)
            .map_err(|_| BytecodeError::Invalid("an octet's length does not fit a length"))?;
        if length > reader.remaining() {
            return Err(BytecodeError::Truncated {
                what: "an octet's bytes",
            });
        }
        let bytes = reader.take(length, "an octet's bytes")?;
        let padded = length.next_multiple_of(4);
        reader.skip(padded - length, "an octet's padding")?;
        constants.octets.push(bytes.to_vec());
    }

    Ok(constants)
}

/// Read one code object.
fn read_object(reader: &mut Reader<'_>) -> Result<Object, BytecodeError> {
    let tag = reader.take(4, "an object's tag")?;
    if tag != b"TJS2" {
        return Err(BytecodeError::BadTag {
            expected: "TJS2",
            found: [tag[0], tag[1], tag[2], tag[3]],
        });
    }
    let declared_size = reader.u32()?;
    let parent = reader.i32()?;
    let name = reader.i32()?;
    let context_type = reader.i32()?;
    let max_variable_count = reader.i32()?;
    let variable_reserve_count = reader.i32()?;
    let max_frame_count = reader.i32()?;
    let argument_count = reader.i32()?;
    let unnamed_argument_base = reader.i32()?;
    let collapse_base = reader.i32()?;
    let property_setter = reader.i32()?;
    let property_getter = reader.i32()?;
    let super_class_getter = reader.i32()?;

    let size = reader.u32()?;
    let count = counted(reader, size, "the source positions")?;
    let mut source_positions = Vec::with_capacity(count);
    for _ in 0..count {
        source_positions.push((reader.u32()?, 0));
    }
    for position in &mut source_positions {
        position.1 = reader.u32()?;
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the code")?;
    let mut code = Vec::with_capacity(count);
    for _ in 0..count {
        code.push(reader.i16()?);
    }
    let padded = count.next_multiple_of(2);
    reader.skip(padded - count, "the code array's padding")?;

    let size = reader.u32()?;
    let count = counted(reader, size, "the constant slots")?;
    let mut data = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = reader.i16()?;
        let index = reader.i16()?;
        data.push((kind, index));
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the super-class getters")?;
    let mut super_class_getter_positions = Vec::with_capacity(count);
    for _ in 0..count {
        super_class_getter_positions.push(reader.u32()?);
    }

    let size = reader.u32()?;
    let count = counted(reader, size, "the members")?;
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.i32()?;
        let object = reader.i32()?;
        properties.push((name, object));
    }

    Ok(Object {
        parent,
        name,
        context_type,
        max_variable_count,
        variable_reserve_count,
        max_frame_count,
        argument_count,
        unnamed_argument_base,
        collapse_base,
        property_setter,
        property_getter,
        super_class_getter,
        source_positions,
        code,
        data,
        super_class_getter_positions,
        properties,
        declared_size,
    })
}

/// A minimal bytecode file, assembled field by field.
///
/// The file holds two strings and one object whose code loads the first
/// string into a register and returns. Every field is written in the order
/// the layout lists: the magic, the version, the file's own size, the
/// constant pool's tag and size, the pool itself, the object section's tag
/// and size, the top-level index, the object's count and the object.
#[cfg(test)]
pub(super) fn fixture() -> Vec<u8> {
    let mut pool = Vec::new();
    // The pool holds the byte, short, integer, long-integer and real arrays,
    // then the strings, then the octets. Every array starts with its count.
    for _ in 0..5 {
        pool.extend_from_slice(&0u32.to_le_bytes());
    }
    pool.extend_from_slice(&2u32.to_le_bytes());
    for text in ["hello", "onMouseUp"] {
        let units: Vec<u16> = text.encode_utf16().collect();
        pool.extend_from_slice(&(units.len() as u32).to_le_bytes());
        for unit in &units {
            pool.extend_from_slice(&unit.to_le_bytes());
        }
        if !units.len().is_multiple_of(2) {
            pool.push(0);
        }
    }
    pool.extend_from_slice(&0u32.to_le_bytes());

    let mut object = Vec::new();
    // The parent, the name, the context kind, the variable counts, the frame
    // count, the argument count, the two argument bases and the three
    // property indices.
    for field in [0i32, 0, 1, 4, 0, 8, 1, 0, 0, -1, -1, -1] {
        object.extend_from_slice(&field.to_le_bytes());
    }
    object.extend_from_slice(&0u32.to_le_bytes());
    object.extend_from_slice(&4u32.to_le_bytes());
    // A constant load into register 1 from the constant slot 0, then a return.
    for value in [1i16, 1, 0, 119] {
        object.extend_from_slice(&value.to_le_bytes());
    }
    object.extend_from_slice(&1u32.to_le_bytes());
    object.extend_from_slice(&3i16.to_le_bytes());
    object.extend_from_slice(&0i16.to_le_bytes());
    object.extend_from_slice(&0u32.to_le_bytes());
    object.extend_from_slice(&0u32.to_le_bytes());

    let mut file = Vec::new();
    file.extend_from_slice(b"TJS2");
    file.extend_from_slice(b"100\0");
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(b"DATA");
    file.extend_from_slice(&((pool.len() + 8) as u32).to_le_bytes());
    file.extend_from_slice(&pool);
    file.extend_from_slice(b"OBJS");
    let section_size = 4 + 4 + 4 + 4 + object.len();
    file.extend_from_slice(&(section_size as u32).to_le_bytes());
    file.extend_from_slice(&0i32.to_le_bytes());
    file.extend_from_slice(&1u32.to_le_bytes());
    file.extend_from_slice(b"TJS2");
    file.extend_from_slice(&(object.len() as u32).to_le_bytes());
    file.extend_from_slice(&object);
    let size = (file.len() as u32).to_le_bytes();
    file[8..12].copy_from_slice(&size);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_parses() {
        let bytes = fixture();
        let file = parse(&bytes).expect("the fixture parses");
        assert_eq!(file.toplevel, 0);
        assert_eq!(file.objects.len(), 1);
        assert_eq!(
            file.constants.strings,
            vec!["hello".to_string(), "onMouseUp".to_string()]
        );
        let object = &file.objects[0];
        assert_eq!(object.name, 0);
        assert_eq!(object.context_type, 1);
        assert_eq!(object.max_variable_count, 4);
        assert_eq!(object.max_frame_count, 8);
        assert_eq!(object.argument_count, 1);
        assert_eq!(object.code, vec![1i16, 1, 0, 119]);
        assert_eq!(object.data, vec![(3i16, 0i16)]);
        assert!(object.properties.is_empty());
        assert!(object.source_positions.is_empty());
    }

    #[test]
    fn a_file_that_is_not_bytecode_is_refused() {
        let error = parse(b"not a bytecode file").expect_err("the file is refused");
        assert!(matches!(&error, BytecodeError::BadMagic(_)), "{error:?}");
    }

    #[test]
    fn a_file_whose_declared_size_differs_from_its_length_is_refused() {
        let mut bytes = fixture();
        bytes.truncate(bytes.len() - 4);
        let error = parse(&bytes).expect_err("the file is refused");
        assert!(
            matches!(&error, BytecodeError::SizeMismatch { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_file_whose_object_section_is_missing_is_refused() {
        let mut bytes = fixture();
        let position = bytes
            .windows(4)
            .position(|window| window == b"OBJS")
            .expect("the tag is there");
        bytes[position] = b'X';
        let error = parse(&bytes).expect_err("the file is refused");
        assert!(matches!(&error, BytecodeError::BadTag { .. }), "{error:?}");
    }
}
