//! Records what the engine answers for the pack format.
//!
//! A pack probe reads the bytes the engine produced without printing the
//! octet itself, which has no text form: the record holds the octet's length,
//! or one byte at a time through its index, or the null answer the empty
//! result makes. An unpack probe reads the array the engine rebuilt through
//! its count, one element, or the element's `typeof`. Every probe is
//! self-contained: it packs its own array, so the record does not depend on
//! the order the probes run in.
//!
//! A probe the language raises records the raise as `!raise`: a refusal is the
//! format's answer, not a gap.
//!
//! The record is written to `tests/fixtures/octet_pack.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example octet_pack_oracle
//! ```

use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use tjs::vm::{
    TJS2_SCRIPT_OK, tjs2_script_compile, tjs2_script_error_message, tjs2_script_free,
    tjs2_script_status,
};
use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/octet_pack_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that defines the probe functions and the arrays with an empty,
/// a null and a nested element.
///
/// The helpers pack their own array, so every probe answers from a fresh
/// octet and the order the probes run in does not matter.
const SETUP: &str = r#"function arrayProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function packLength(array, format) {
    var packed = array.pack(format);
    if (typeof (packed) == "Octet") return packed.length;
    return -1;
}
function packByte(array, format, index) {
    return array.pack(format)[index];
}
function unpackCount(array, packFormat, unpackFormat) {
    return array.pack(packFormat).unpack(unpackFormat).count;
}
function unpackElement(array, packFormat, unpackFormat, index) {
    return array.pack(packFormat).unpack(unpackFormat)[index];
}
function unpackType(array, packFormat, unpackFormat, index) {
    return typeof (array.pack(packFormat).unpack(unpackFormat)[index]);
}
var v = new Array(); v[0] = void;
var n = new Array(); n[0] = null;
var nested = new Array(); nested[0] = [1, 2];
"#;

/// The probes: the id the check names the rule by, and the expression.
const PROBES: &[(&str, &str)] = &[
    ("pack_empty_len", "packLength([], \"i\")"),
    ("pack_empty_type", "typeof ([].pack(\"i\"))"),
    ("pack_empty_format_len", "packLength([1], \"\")"),
    ("pack_c3_len", "packLength([1, -1, 255], \"c3\")"),
    ("pack_c3_0", "packByte([1, -1, 255], \"c3\", 0)"),
    ("pack_c3_1", "packByte([1, -1, 255], \"c3\", 1)"),
    ("pack_c3_2", "packByte([1, -1, 255], \"c3\", 2)"),
    ("pack_C3_len", "packLength([256, -1, 65], \"C3\")"),
    ("pack_C3_0", "packByte([256, -1, 65], \"C3\", 0)"),
    ("pack_C3_1", "packByte([256, -1, 65], \"C3\", 1)"),
    ("pack_C3_2", "packByte([256, -1, 65], \"C3\", 2)"),
    ("pack_s2_len", "packLength([-2, 258], \"s2\")"),
    ("pack_s2_0", "packByte([-2, 258], \"s2\", 0)"),
    ("pack_s2_1", "packByte([-2, 258], \"s2\", 1)"),
    ("pack_s2_2", "packByte([-2, 258], \"s2\", 2)"),
    ("pack_s2_3", "packByte([-2, 258], \"s2\", 3)"),
    ("pack_S2_len", "packLength([65535, 258], \"S2\")"),
    ("pack_S2_0", "packByte([65535, 258], \"S2\", 0)"),
    ("pack_S2_1", "packByte([65535, 258], \"S2\", 1)"),
    ("pack_S2_2", "packByte([65535, 258], \"S2\", 2)"),
    ("pack_S2_3", "packByte([65535, 258], \"S2\", 3)"),
    ("pack_i2_len", "packLength([-2, 16909060], \"i2\")"),
    ("pack_i2_0", "packByte([-2, 16909060], \"i2\", 0)"),
    ("pack_i2_1", "packByte([-2, 16909060], \"i2\", 1)"),
    ("pack_i2_2", "packByte([-2, 16909060], \"i2\", 2)"),
    ("pack_i2_3", "packByte([-2, 16909060], \"i2\", 3)"),
    ("pack_i2_4", "packByte([-2, 16909060], \"i2\", 4)"),
    ("pack_i2_5", "packByte([-2, 16909060], \"i2\", 5)"),
    ("pack_i2_6", "packByte([-2, 16909060], \"i2\", 6)"),
    ("pack_i2_7", "packByte([-2, 16909060], \"i2\", 7)"),
    ("pack_I2_len", "packLength([4294967295, 16909060], \"I2\")"),
    ("pack_I2_0", "packByte([4294967295, 16909060], \"I2\", 0)"),
    ("pack_I2_1", "packByte([4294967295, 16909060], \"I2\", 1)"),
    ("pack_I2_2", "packByte([4294967295, 16909060], \"I2\", 2)"),
    ("pack_I2_3", "packByte([4294967295, 16909060], \"I2\", 3)"),
    ("pack_I2_4", "packByte([4294967295, 16909060], \"I2\", 4)"),
    ("pack_I2_5", "packByte([4294967295, 16909060], \"I2\", 5)"),
    ("pack_I2_6", "packByte([4294967295, 16909060], \"I2\", 6)"),
    ("pack_I2_7", "packByte([4294967295, 16909060], \"I2\", 7)"),
    ("pack_l1_len", "packLength([-2], \"l\")"),
    ("pack_l1_0", "packByte([-2], \"l\", 0)"),
    ("pack_l1_1", "packByte([-2], \"l\", 1)"),
    ("pack_l1_2", "packByte([-2], \"l\", 2)"),
    ("pack_l1_3", "packByte([-2], \"l\", 3)"),
    ("pack_L1_len", "packLength([4294967295], \"L\")"),
    ("pack_L1_0", "packByte([4294967295], \"L\", 0)"),
    ("pack_L1_1", "packByte([4294967295], \"L\", 1)"),
    ("pack_L1_2", "packByte([4294967295], \"L\", 2)"),
    ("pack_L1_3", "packByte([4294967295], \"L\", 3)"),
    ("pack_n1_len", "packLength([258], \"n\")"),
    ("pack_n1_0", "packByte([258], \"n\", 0)"),
    ("pack_n1_1", "packByte([258], \"n\", 1)"),
    ("pack_N1_len", "packLength([16909060], \"N\")"),
    ("pack_N1_0", "packByte([16909060], \"N\", 0)"),
    ("pack_N1_1", "packByte([16909060], \"N\", 1)"),
    ("pack_N1_2", "packByte([16909060], \"N\", 2)"),
    ("pack_N1_3", "packByte([16909060], \"N\", 3)"),
    ("pack_v1_len", "packLength([258], \"v\")"),
    ("pack_v1_0", "packByte([258], \"v\", 0)"),
    ("pack_v1_1", "packByte([258], \"v\", 1)"),
    ("pack_V1_len", "packLength([16909060], \"V\")"),
    ("pack_V1_0", "packByte([16909060], \"V\", 0)"),
    ("pack_V1_1", "packByte([16909060], \"V\", 1)"),
    ("pack_V1_2", "packByte([16909060], \"V\", 2)"),
    ("pack_V1_3", "packByte([16909060], \"V\", 3)"),
    ("pack_d1_len", "packLength([1.5], \"d\")"),
    ("pack_d1_0", "packByte([1.5], \"d\", 0)"),
    ("pack_d1_1", "packByte([1.5], \"d\", 1)"),
    ("pack_d1_2", "packByte([1.5], \"d\", 2)"),
    ("pack_d1_3", "packByte([1.5], \"d\", 3)"),
    ("pack_d1_4", "packByte([1.5], \"d\", 4)"),
    ("pack_d1_5", "packByte([1.5], \"d\", 5)"),
    ("pack_d1_6", "packByte([1.5], \"d\", 6)"),
    ("pack_d1_7", "packByte([1.5], \"d\", 7)"),
    ("pack_f1_len", "packLength([1.5], \"f\")"),
    ("pack_f1_0", "packByte([1.5], \"f\", 0)"),
    ("pack_f1_1", "packByte([1.5], \"f\", 1)"),
    ("pack_f1_2", "packByte([1.5], \"f\", 2)"),
    ("pack_f1_3", "packByte([1.5], \"f\", 3)"),
    ("pack_f_tenth_len", "packLength([0.1], \"f\")"),
    ("pack_f_tenth_0", "packByte([0.1], \"f\", 0)"),
    ("pack_f_tenth_1", "packByte([0.1], \"f\", 1)"),
    ("pack_f_tenth_2", "packByte([0.1], \"f\", 2)"),
    ("pack_f_tenth_3", "packByte([0.1], \"f\", 3)"),
    ("pack_f_overflow_len", "packLength([1e40], \"f\")"),
    ("pack_f_overflow_0", "packByte([1e40], \"f\", 0)"),
    ("pack_f_overflow_1", "packByte([1e40], \"f\", 1)"),
    ("pack_f_overflow_2", "packByte([1e40], \"f\", 2)"),
    ("pack_f_overflow_3", "packByte([1e40], \"f\", 3)"),
    ("pack_f_negative_zero_len", "packLength([-0.0], \"f\")"),
    ("pack_f_negative_zero_0", "packByte([-0.0], \"f\", 0)"),
    ("pack_f_negative_zero_1", "packByte([-0.0], \"f\", 1)"),
    ("pack_f_negative_zero_2", "packByte([-0.0], \"f\", 2)"),
    ("pack_f_negative_zero_3", "packByte([-0.0], \"f\", 3)"),
    ("pack_clamp_len", "packLength([1], \"i2\")"),
    ("pack_clamp_0", "packByte([1], \"i2\", 0)"),
    ("pack_clamp_1", "packByte([1], \"i2\", 1)"),
    ("pack_clamp_2", "packByte([1], \"i2\", 2)"),
    ("pack_clamp_3", "packByte([1], \"i2\", 3)"),
    ("pack_zero_count_len", "packLength([1], \"i0i\")"),
    ("pack_zero_count_0", "packByte([1], \"i0i\", 0)"),
    ("pack_zero_count_1", "packByte([1], \"i0i\", 1)"),
    ("pack_zero_count_2", "packByte([1], \"i0i\", 2)"),
    ("pack_zero_count_3", "packByte([1], \"i0i\", 3)"),
    ("pack_star_len", "packLength([1, 2], \"i*\")"),
    ("pack_star_0", "packByte([1, 2], \"i*\", 0)"),
    ("pack_star_1", "packByte([1, 2], \"i*\", 1)"),
    ("pack_star_2", "packByte([1, 2], \"i*\", 2)"),
    ("pack_star_3", "packByte([1, 2], \"i*\", 3)"),
    ("pack_star_4", "packByte([1, 2], \"i*\", 4)"),
    ("pack_star_5", "packByte([1, 2], \"i*\", 5)"),
    ("pack_star_6", "packByte([1, 2], \"i*\", 6)"),
    ("pack_star_7", "packByte([1, 2], \"i*\", 7)"),
    ("pack_text_number_len", "packLength([\"12\"], \"i\")"),
    ("pack_text_number_0", "packByte([\"12\"], \"i\", 0)"),
    ("pack_text_number_1", "packByte([\"12\"], \"i\", 1)"),
    ("pack_text_number_2", "packByte([\"12\"], \"i\", 2)"),
    ("pack_text_number_3", "packByte([\"12\"], \"i\", 3)"),
    ("pack_real_trunc_len", "packLength([1.9], \"i\")"),
    ("pack_real_trunc_0", "packByte([1.9], \"i\", 0)"),
    ("pack_real_trunc_1", "packByte([1.9], \"i\", 1)"),
    ("pack_real_trunc_2", "packByte([1.9], \"i\", 2)"),
    ("pack_real_trunc_3", "packByte([1.9], \"i\", 3)"),
    ("pack_void_len", "packLength(v, \"i\")"),
    ("pack_void_0", "packByte(v, \"i\", 0)"),
    ("pack_void_1", "packByte(v, \"i\", 1)"),
    ("pack_void_2", "packByte(v, \"i\", 2)"),
    ("pack_void_3", "packByte(v, \"i\", 3)"),
    ("pack_x_len", "packLength([1], \"x2i\")"),
    ("pack_x_0", "packByte([1], \"x2i\", 0)"),
    ("pack_x_1", "packByte([1], \"x2i\", 1)"),
    ("pack_x_2", "packByte([1], \"x2i\", 2)"),
    ("pack_x_3", "packByte([1], \"x2i\", 3)"),
    ("pack_x_4", "packByte([1], \"x2i\", 4)"),
    ("pack_x_5", "packByte([1], \"x2i\", 5)"),
    ("pack_trailing_x_len", "packLength([1], \"ix2\")"),
    ("pack_trailing_x_0", "packByte([1], \"ix2\", 0)"),
    ("pack_trailing_x_1", "packByte([1], \"ix2\", 1)"),
    ("pack_trailing_x_2", "packByte([1], \"ix2\", 2)"),
    ("pack_trailing_x_3", "packByte([1], \"ix2\", 3)"),
    ("pack_backup_len", "packLength([1, 2], \"iX1i\")"),
    ("pack_backup_0", "packByte([1, 2], \"iX1i\", 0)"),
    ("pack_backup_1", "packByte([1, 2], \"iX1i\", 1)"),
    ("pack_backup_2", "packByte([1, 2], \"iX1i\", 2)"),
    ("pack_backup_3", "packByte([1, 2], \"iX1i\", 3)"),
    ("pack_backup_4", "packByte([1, 2], \"iX1i\", 4)"),
    ("pack_backup_5", "packByte([1, 2], \"iX1i\", 5)"),
    ("pack_backup_6", "packByte([1, 2], \"iX1i\", 6)"),
    ("pack_fill_len", "packLength([1], \"@4i\")"),
    ("pack_fill_0", "packByte([1], \"@4i\", 0)"),
    ("pack_fill_1", "packByte([1], \"@4i\", 1)"),
    ("pack_fill_2", "packByte([1], \"@4i\", 2)"),
    ("pack_fill_3", "packByte([1], \"@4i\", 3)"),
    ("pack_fill_4", "packByte([1], \"@4i\", 4)"),
    ("pack_fill_5", "packByte([1], \"@4i\", 5)"),
    ("pack_fill_6", "packByte([1], \"@4i\", 6)"),
    ("pack_fill_7", "packByte([1], \"@4i\", 7)"),
    ("pack_fill_no_truncate_len", "packLength([1, 2], \"i@2i\")"),
    ("pack_fill_no_truncate_0", "packByte([1, 2], \"i@2i\", 0)"),
    ("pack_fill_no_truncate_1", "packByte([1, 2], \"i@2i\", 1)"),
    ("pack_fill_no_truncate_2", "packByte([1, 2], \"i@2i\", 2)"),
    ("pack_fill_no_truncate_3", "packByte([1, 2], \"i@2i\", 3)"),
    ("pack_fill_no_truncate_4", "packByte([1, 2], \"i@2i\", 4)"),
    ("pack_fill_no_truncate_5", "packByte([1, 2], \"i@2i\", 5)"),
    ("pack_fill_no_truncate_6", "packByte([1, 2], \"i@2i\", 6)"),
    ("pack_fill_no_truncate_7", "packByte([1, 2], \"i@2i\", 7)"),
    ("pack_a5_len", "packLength([\"ab\"], \"a5\")"),
    ("pack_a5_0", "packByte([\"ab\"], \"a5\", 0)"),
    ("pack_a5_1", "packByte([\"ab\"], \"a5\", 1)"),
    ("pack_a5_2", "packByte([\"ab\"], \"a5\", 2)"),
    ("pack_a5_3", "packByte([\"ab\"], \"a5\", 3)"),
    ("pack_a5_4", "packByte([\"ab\"], \"a5\", 4)"),
    ("pack_A5_len", "packLength([\"ab\"], \"A5\")"),
    ("pack_A5_0", "packByte([\"ab\"], \"A5\", 0)"),
    ("pack_A5_1", "packByte([\"ab\"], \"A5\", 1)"),
    ("pack_A5_2", "packByte([\"ab\"], \"A5\", 2)"),
    ("pack_A5_3", "packByte([\"ab\"], \"A5\", 3)"),
    ("pack_A5_4", "packByte([\"ab\"], \"A5\", 4)"),
    ("pack_a3_len", "packLength([\"abcdef\"], \"a3\")"),
    ("pack_a3_0", "packByte([\"abcdef\"], \"a3\", 0)"),
    ("pack_a3_1", "packByte([\"abcdef\"], \"a3\", 1)"),
    ("pack_a3_2", "packByte([\"abcdef\"], \"a3\", 2)"),
    ("pack_astar_len", "packLength([\"ab\", \"cd\"], \"a*\")"),
    ("pack_astar_0", "packByte([\"ab\", \"cd\"], \"a*\", 0)"),
    ("pack_astar_1", "packByte([\"ab\", \"cd\"], \"a*\", 1)"),
    ("pack_astar_2", "packByte([\"ab\", \"cd\"], \"a*\", 2)"),
    ("pack_astar_3", "packByte([\"ab\", \"cd\"], \"a*\", 3)"),
    ("pack_a_utf16_len", "packLength([\"あ\"], \"a2\")"),
    ("pack_a_utf16_0", "packByte([\"あ\"], \"a2\", 0)"),
    ("pack_a_utf16_1", "packByte([\"あ\"], \"a2\", 1)"),
    ("pack_b8_len", "packLength([\"10110010\"], \"b8\")"),
    ("pack_b8_0", "packByte([\"10110010\"], \"b8\", 0)"),
    ("pack_B8_len", "packLength([\"10110010\"], \"B8\")"),
    ("pack_B8_0", "packByte([\"10110010\"], \"B8\", 0)"),
    ("pack_b3_len", "packLength([\"101\"], \"b3\")"),
    ("pack_b3_0", "packByte([\"101\"], \"b3\", 0)"),
    ("pack_B3_len", "packLength([\"101\"], \"B3\")"),
    ("pack_B3_0", "packByte([\"101\"], \"B3\", 0)"),
    ("pack_b_short_len", "packLength([\"101\"], \"b8\")"),
    ("pack_b_short_0", "packByte([\"101\"], \"b8\", 0)"),
    ("pack_h4_len", "packLength([\"a1b2\"], \"h4\")"),
    ("pack_h4_0", "packByte([\"a1b2\"], \"h4\", 0)"),
    ("pack_h4_1", "packByte([\"a1b2\"], \"h4\", 1)"),
    ("pack_H4_len", "packLength([\"a1b2\"], \"H4\")"),
    ("pack_H4_0", "packByte([\"a1b2\"], \"H4\", 0)"),
    ("pack_H4_1", "packByte([\"a1b2\"], \"H4\", 1)"),
    ("pack_h_lower_len", "packLength([\"f0\"], \"h2\")"),
    ("pack_h_lower_0", "packByte([\"f0\"], \"h2\", 0)"),
    ("pack_m_len", "packLength([\"SGVsbG8=\"], \"m\")"),
    ("pack_m_0", "packByte([\"SGVsbG8=\"], \"m\", 0)"),
    ("pack_m_1", "packByte([\"SGVsbG8=\"], \"m\", 1)"),
    ("pack_m_2", "packByte([\"SGVsbG8=\"], \"m\", 2)"),
    ("pack_m_3", "packByte([\"SGVsbG8=\"], \"m\", 3)"),
    ("pack_m_4", "packByte([\"SGVsbG8=\"], \"m\", 4)"),
    ("pack_p_len", "packLength([1], \"p\")"),
    ("pack_P_len", "packLength([1], \"P\")"),
    ("pack_u_refused", "packLength([1], \"u\")"),
    ("pack_w_refused", "packLength([1], \"w\")"),
    ("pack_unknown_refused", "packLength([1], \"q\")"),
    ("pack_bit_refused", "packLength([\"102\"], \"b3\")"),
    ("pack_hex_refused", "packLength([\"F0\"], \"h2\")"),
    ("pack_null_refused", "packLength(n, \"i\")"),
    ("pack_nested_refused", "packLength(nested, \"i\")"),
    (
        "unpack_c3_count",
        "unpackCount([1, -1, 255], \"c3\", \"c3\")",
    ),
    (
        "unpack_c3_v1",
        "unpackElement([1, -1, 255], \"c3\", \"c3\", 1)",
    ),
    (
        "unpack_c3_t1",
        "unpackType([1, -1, 255], \"c3\", \"c3\", 1)",
    ),
    (
        "unpack_C3_count",
        "unpackCount([256, -1, 65], \"C3\", \"C3\")",
    ),
    (
        "unpack_C3_v0",
        "unpackElement([256, -1, 65], \"C3\", \"C3\", 0)",
    ),
    (
        "unpack_C3_v2",
        "unpackElement([256, -1, 65], \"C3\", \"C3\", 2)",
    ),
    ("unpack_s2_count", "unpackCount([-2, 258], \"s2\", \"s2\")"),
    (
        "unpack_s2_v1",
        "unpackElement([-2, 258], \"s2\", \"s2\", 1)",
    ),
    (
        "unpack_S2_count",
        "unpackCount([65535, 258], \"S2\", \"S2\")",
    ),
    (
        "unpack_S2_v0",
        "unpackElement([65535, 258], \"S2\", \"S2\", 0)",
    ),
    (
        "unpack_i2_count",
        "unpackCount([-2, 16909060], \"i2\", \"i2\")",
    ),
    (
        "unpack_i2_v1",
        "unpackElement([-2, 16909060], \"i2\", \"i2\", 1)",
    ),
    (
        "unpack_I2_count",
        "unpackCount([4294967295, 16909060], \"I2\", \"I2\")",
    ),
    (
        "unpack_I2_v0",
        "unpackElement([4294967295, 16909060], \"I2\", \"I2\", 0)",
    ),
    ("unpack_n1_count", "unpackCount([258], \"n\", \"n\")"),
    ("unpack_n1_v0", "unpackElement([258], \"n\", \"n\", 0)"),
    ("unpack_N1_count", "unpackCount([16909060], \"N\", \"N\")"),
    ("unpack_N1_v0", "unpackElement([16909060], \"N\", \"N\", 0)"),
    ("unpack_v1_count", "unpackCount([258], \"v\", \"v\")"),
    ("unpack_v1_v0", "unpackElement([258], \"v\", \"v\", 0)"),
    ("unpack_V1_count", "unpackCount([16909060], \"V\", \"V\")"),
    ("unpack_V1_v0", "unpackElement([16909060], \"V\", \"V\", 0)"),
    ("unpack_d1_count", "unpackCount([1.5], \"d\", \"d\")"),
    ("unpack_d1_v0", "unpackElement([1.5], \"d\", \"d\", 0)"),
    ("unpack_d1_t0", "unpackType([1.5], \"d\", \"d\", 0)"),
    ("unpack_f1_count", "unpackCount([1.5], \"f\", \"f\")"),
    ("unpack_f1_v0", "unpackElement([1.5], \"f\", \"f\", 0)"),
    ("unpack_a5_count", "unpackCount([\"ab\"], \"a5\", \"a5\")"),
    ("unpack_a5_v0", "unpackElement([\"ab\"], \"a5\", \"a5\", 0)"),
    ("unpack_a5_t0", "unpackType([\"ab\"], \"a5\", \"a5\", 0)"),
    ("unpack_A5_count", "unpackCount([\"ab\"], \"A5\", \"A5\")"),
    ("unpack_A5_v0", "unpackElement([\"ab\"], \"A5\", \"A5\", 0)"),
    ("unpack_A5_t0", "unpackType([\"ab\"], \"A5\", \"A5\", 0)"),
    (
        "unpack_astar_count",
        "unpackCount([\"ab\", \"cd\"], \"a*\", \"a*\")",
    ),
    (
        "unpack_astar_v0",
        "unpackElement([\"ab\", \"cd\"], \"a*\", \"a*\", 0)",
    ),
    (
        "unpack_a_utf8_count",
        "unpackCount([227, 129, 130], \"C3\", \"a3\")",
    ),
    (
        "unpack_a_utf8_v0",
        "unpackElement([227, 129, 130], \"C3\", \"a3\", 0)",
    ),
    (
        "unpack_b8_count",
        "unpackCount([\"10110010\"], \"b8\", \"b8\")",
    ),
    (
        "unpack_b8_v0",
        "unpackElement([\"10110010\"], \"b8\", \"b8\", 0)",
    ),
    (
        "unpack_B8_count",
        "unpackCount([\"10110010\"], \"B8\", \"B8\")",
    ),
    (
        "unpack_B8_v0",
        "unpackElement([\"10110010\"], \"B8\", \"B8\", 0)",
    ),
    ("unpack_h4_count", "unpackCount([\"a1b2\"], \"h4\", \"h4\")"),
    (
        "unpack_h4_v0",
        "unpackElement([\"a1b2\"], \"h4\", \"h4\", 0)",
    ),
    ("unpack_H4_count", "unpackCount([\"a1b2\"], \"H4\", \"H4\")"),
    (
        "unpack_H4_v0",
        "unpackElement([\"a1b2\"], \"H4\", \"H4\", 0)",
    ),
    ("unpack_x_count", "unpackCount([1], \"i\", \"x1i\")"),
    ("unpack_x_v0", "unpackElement([1], \"i\", \"x1i\", 0)"),
    ("unpack_at_count", "unpackCount([1, 2], \"i2\", \"@4i\")"),
    ("unpack_at_v0", "unpackElement([1, 2], \"i2\", \"@4i\", 0)"),
    ("unpack_X_count", "unpackCount([1], \"i\", \"iX1C\")"),
    ("unpack_X_v0", "unpackElement([1], \"i\", \"iX1C\", 0)"),
    ("unpack_X_v1", "unpackElement([1], \"i\", \"iX1C\", 1)"),
    (
        "unpack_partial_count",
        "unpackCount([1, 2, 3], \"C3\", \"i\")",
    ),
    (
        "unpack_partial_v0",
        "unpackElement([1, 2, 3], \"C3\", \"i\", 0)",
    ),
    ("unpack_clamp_count", "unpackCount([1, 2], \"C2\", \"C5\")"),
    (
        "unpack_clamp_v1",
        "unpackElement([1, 2], \"C2\", \"C5\", 1)",
    ),
    ("unpack_m_count", "unpackCount([\"Hello\"], \"a5\", \"m\")"),
    (
        "unpack_m_v0",
        "unpackElement([\"Hello\"], \"a5\", \"m\", 0)",
    ),
    ("unpack_xstar_count", "unpackCount([1, 2], \"i2\", \"x*\")"),
    ("unpack_Xstar_count", "unpackCount([1, 2], \"i2\", \"X*i\")"),
    (
        "unpack_Xstar_v0",
        "unpackElement([1, 2], \"i2\", \"X*i\", 0)",
    ),
    ("unpack_atstar_count", "unpackCount([1, 2], \"i2\", \"@*\")"),
    (
        "unpack_empty_template_count",
        "unpackCount([1], \"i\", \"\")",
    ),
    (
        "unpack_a_bad_utf8_refused",
        "unpackCount([255], \"C1\", \"a1\")",
    ),
    (
        "unpack_a_four_byte_refused",
        "unpackCount([240], \"C1\", \"a1\")",
    ),
    ("unpack_u_refused", "unpackCount([1], \"i\", \"u\")"),
    ("unpack_w_refused", "unpackCount([1], \"i\", \"w\")"),
    ("unpack_p_refused", "unpackCount([1], \"i\", \"p\")"),
    ("unpack_P_refused", "unpackCount([1], \"i\", \"P\")"),
    ("unpack_unknown_refused", "unpackCount([1], \"i\", \"q\")"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "octet_pack") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let call = format!("arrayProbe({expression})");
        let answer = answer(&engine, &call, probe);
        let (kind, text) = match answer {
            Answer::Answered(kind, text) => (kind, escaped(&text)),
            Answer::Raised => (String::from(RAISED), String::new()),
        };
        table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
    }
    let path = fixture_path();
    let temporary = path.with_extension("txt.tmp");
    if let Err(error) = fs::write(&temporary, table) {
        fail(format_args!(
            "the temporary record {} is not writable: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        fail(format_args!(
            "the record could not be renamed to {}: {error}",
            path.display()
        ));
    }
    println!("wrote {}", path.display());
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/octet_pack.txt")
}

/// What one probe answered.
enum Answer {
    /// The kind the answer's `typeof` reports, and its text.
    Answered(String, String),
    /// The probe raised.
    Raised,
}

/// Evaluates one probe, which must compile.
///
/// The call is compiled first, so a probe the front end refuses is told apart
/// from a raise: it never reaches a rule, and the recorder stops rather than
/// record it as the rule's answer.
fn answer(engine: &Tjs2Engine, call: &str, probe: &str) -> Answer {
    if let Err(message) = compiles(call) {
        fail(format_args!(
            "probe {probe}: the expression does not compile (!compile): {message}"
        ));
    }
    match engine.eval(call, "octet_pack") {
        Ok(TjsValue::String(text)) => match text.split_once(':') {
            Some((kind, value)) => Answer::Answered(kind.to_owned(), value.to_owned()),
            None => fail(format_args!(
                "probe {probe}: the answer has no kind and text: {text:?}"
            )),
        },
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(_raised) => Answer::Raised,
    }
}

/// Whether the front end compiles the call, and why it does not.
fn compiles(expression: &str) -> Result<(), String> {
    let units: Vec<u16> = expression.encode_utf16().collect();
    let length = u32::try_from(units.len()).expect("a probe expression fits a 32-bit length");
    // SAFETY: the units are readable for `length` code units, the null values
    // pointer means "no conditional-compilation values", and the handle is
    // freed before this returns.
    unsafe {
        let script = tjs2_script_compile(units.as_ptr(), length, 1, 1, ptr::null());
        if script.is_null() {
            return Err(String::from("the front end answered no handle"));
        }
        let status = tjs2_script_status(script);
        if status == TJS2_SCRIPT_OK {
            tjs2_script_free(script);
            return Ok(());
        }
        let message = tjs2_script_error_message(script);
        let message = if message.is_null() {
            String::from("the front end gave no message")
        } else {
            // SAFETY: the handle owns a NUL-terminated message, live until the
            // handle is freed below.
            CStr::from_ptr(message).to_string_lossy().into_owned()
        };
        tjs2_script_free(script);
        Err(message)
    }
}

/// The text escaped so that one probe stays on one line.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\\' => escaped.push_str("\\\\"),
            character if character < ' ' => {
                escaped.push_str(&format!("\\x{:02x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("octet_pack_oracle: {message}");
    std::process::exit(1)
}
