//! The regular-expression engine's C interface.
//!
//! The engine is compiled and linked by this crate's build script, so its
//! symbols are available to the Rust side without another dependency. This
//! module is the whole binding: a compiled expression, a match region, and the
//! three calls that create, search and free them. The types the library uses
//! are named here only as far as their pointers cross the interface.
//!
//! The search takes UTF-16 code units and encodes them little-endian, the
//! encoding the engine is configured for. A region's bounds are byte offsets
//! into that encoding, relative to the start the search was given; the caller
//! divides them by two to read code units.
//!
//! The engine initializes itself on the first compile, and this module never
//! tears it down: other users of the library may still hold an expression.

use std::ffi::c_void;
use std::ptr;
use std::slice;

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

/// The library's region object.
#[repr(C)]
struct RegionHeader {
    /// The number of slots the region allocated.
    allocated: i32,
    /// The number of slots the last search filled.
    num_regs: i32,
    /// The byte offset each slot starts at, or -1.
    beg: *mut i32,
    /// The byte offset each slot ends at, or -1.
    end: *mut i32,
    /// The capture history root, unused here.
    history_root: *mut c_void,
}

/// The error detail a failed compile fills.
#[repr(C)]
struct ErrorInfo {
    /// The encoding the compile used.
    encoding: *mut c_void,
    /// The start of the part the compile refused.
    parameter: *mut u8,
    /// The end of the part the compile refused.
    parameter_end: *mut u8,
}

unsafe extern "C" {
    /// The UTF-16 little-endian encoding object.
    static mut OnigEncodingUTF16_LE: c_void;
    /// The Perl syntax object.
    static mut OnigSyntaxPerl: c_void;

    /// Compiles a pattern.
    fn onig_new(
        expression: *mut *mut c_void,
        pattern: *const u8,
        pattern_end: *const u8,
        option: u32,
        encoding: *mut c_void,
        syntax: *mut c_void,
        error: *mut ErrorInfo,
    ) -> i32;
    /// Frees a compiled pattern.
    fn onig_free(expression: *mut c_void);
    /// Searches a compiled pattern in a target.
    fn onig_search(
        expression: *mut c_void,
        target: *const u8,
        target_end: *const u8,
        start: *const u8,
        range: *const u8,
        region: *mut RegionHeader,
        option: u32,
    ) -> i32;
    /// Allocates a region.
    fn onig_region_new() -> *mut RegionHeader;
    /// Frees a region, and its slots when `free_self` is zero.
    fn onig_region_free(region: *mut RegionHeader, free_self: i32);
    /// Writes a compile error's message.
    fn onig_error_code_to_str(message: *mut u8, code: i32, ...) -> i32;
}

/// A compiled expression.
pub(crate) struct Expression {
    /// The library's handle.
    raw: *mut c_void,
}

impl Expression {
    /// Compiles `pattern` with `options`.
    ///
    /// The failure is the library's own message for the pattern, which is the
    /// text a script reads from the raised exception.
    pub(crate) fn compile(pattern: &[u16], options: u32) -> Result<Self, String> {
        let bytes = encode(pattern);
        let mut raw = ptr::null_mut();
        let mut error = ErrorInfo {
            encoding: ptr::null_mut(),
            parameter: ptr::null_mut(),
            parameter_end: ptr::null_mut(),
        };
        // SAFETY: the pattern buffer is readable for its length, the two
        // library objects are the ones linked with this crate, and the error
        // record is writable for the call.
        let code = unsafe {
            onig_new(
                &mut raw,
                bytes.as_ptr(),
                bytes.as_ptr().add(bytes.len()),
                options & OPTION_MASK,
                (&raw mut OnigEncodingUTF16_LE).cast(),
                (&raw mut OnigSyntaxPerl).cast(),
                &mut error,
            )
        };
        if code != 0 {
            let message = error_message(code, &error);
            // SAFETY: a failed compile may still have allocated a handle, and
            // the library owns the free.
            unsafe {
                if !raw.is_null() {
                    onig_free(raw);
                }
            }
            return Err(message);
        }
        if raw.is_null() {
            return Err(String::from("the expression engine allocated no handle"));
        }
        Ok(Self { raw })
    }

    /// Searches `target` from `start` through `end`, both in code units.
    ///
    /// The answer is the region the search filled, or `None` when the pattern
    /// did not match or the target could not be searched. A negative search
    /// result is not a match, which is the rule the class's `Match` applies.
    pub(crate) fn search(
        &self,
        target: &[u8],
        start: usize,
        end: usize,
    ) -> Result<Option<Region>, RegionError> {
        let Some(start_bytes) = start.checked_mul(2) else {
            return Err(RegionError);
        };
        let Some(end_bytes) = end.checked_mul(2) else {
            return Err(RegionError);
        };
        if end_bytes > target.len() || start_bytes > end_bytes {
            return Err(RegionError);
        }
        // SAFETY: the region is allocated by the library and freed below, and
        // the four pointers are within the target buffer because the bounds
        // were checked.
        let region = unsafe { onig_region_new() };
        if region.is_null() {
            return Err(RegionError);
        }
        let base = target.as_ptr();
        // SAFETY: `start_bytes` and `end_bytes` are within `target`, and the
        // region is the one allocated above.
        let result = unsafe {
            onig_search(
                self.raw,
                base.add(start_bytes),
                base.add(end_bytes),
                base.add(start_bytes),
                base.add(end_bytes),
                region,
                OPTION_NONE,
            )
        };
        let answer = if result >= 0 {
            // SAFETY: a successful search filled the region, whose two slot
            // arrays are readable for the count the library reports.
            unsafe { Region::read(region) }.map(Some)
        } else {
            Ok(None)
        };
        // SAFETY: the region was allocated by the library and is freed once.
        unsafe {
            onig_region_free(region, 1);
        }
        answer
    }
}

impl Drop for Expression {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful compile and is freed once.
        unsafe {
            onig_free(self.raw);
        }
    }
}

/// The result of a search: the byte offsets of every captured slot.
pub(crate) struct Region {
    /// The start of each slot, or -1.
    beg: Vec<i32>,
    /// The end of each slot, or -1.
    end: Vec<i32>,
}

impl Region {
    /// Reads the library's region into owned slots.
    ///
    /// # Safety
    ///
    /// `region` must be live and filled, and its slot arrays must be readable
    /// for `num_regs` entries.
    unsafe fn read(region: *const RegionHeader) -> Result<Self, RegionError> {
        // SAFETY: the caller guarantees that `region` is live and filled, so
        // its header is readable.
        let count = unsafe { (*region).num_regs.max(0) };
        let count = usize::try_from(count).map_err(|_| RegionError)?;
        if count == 0 {
            return Ok(Self {
                beg: Vec::new(),
                end: Vec::new(),
            });
        }
        // SAFETY: the caller guarantees both arrays are readable for `count`
        // entries.
        unsafe {
            Ok(Self {
                beg: slice::from_raw_parts((*region).beg, count).to_vec(),
                end: slice::from_raw_parts((*region).end, count).to_vec(),
            })
        }
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

/// The little-endian bytes of `units`.
pub(crate) fn encode(units: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(units.len().saturating_mul(2));
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// The message of a compile failure.
fn error_message(code: i32, error: &ErrorInfo) -> String {
    let mut message = [0u8; 90];
    // SAFETY: the buffer is 90 bytes, the length the header declares, and the
    // error record is the one the compile filled.
    unsafe {
        onig_error_code_to_str(message.as_mut_ptr(), code, error as *const ErrorInfo);
    }
    let end = message
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(message.len());
    String::from_utf8_lossy(&message[..end]).into_owned()
}

/// A region the library reported with a count this target cannot hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RegionError;

/// The option value that asks for nothing.
const OPTION_NONE: u32 = 0;
