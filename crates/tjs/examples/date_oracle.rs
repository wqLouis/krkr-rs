//! Records what the engine answers for a date's members and for its grammar.
//!
//! A script reaches a date through its class's constructor, its accessors and
//! setters, its millisecond pair and its `parse` member. This recorder builds
//! dates at a fixed zone (`TZ=JST-9`, nine hours east of UTC and without a
//! daylight-saving rule), asks for the fields, the epoch and the text grammar
//! of a fixed probe table, and writes one `probe<TAB>kind<TAB>text` line per
//! probe. The check in `tests/date_members.rs` replays the same probes through
//! the calendar and the grammar and compares.
//!
//! A probe the language raises records the raise as `!raise`: a refusal is an
//! answer, not a gap in the record. The record carries the zone's offset in
//! its `zone_offset` probe, so the check reads the zone from the record rather
//! than assuming the machine's.
//!
//! The record is written to `tests/fixtures/date_members.txt` atomically: the
//! whole table is written to a temporary file beside the fixture and renamed
//! over it, so a failed run never leaves a half record behind.
//!
//! ```sh
//! TZ=JST-9 cargo run -p tjs --example date_oracle
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/date_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The minute offset the record is taken at: `JST-9` is nine hours east of
/// UTC, so the class answers 540 minutes west.
const RECORDED_MINUTES_WEST: i64 = -540;

/// The script that builds the probe helpers.
///
/// Every helper is a named function because a script cannot declare one inside
/// an anonymous body. A helper returns a text, so one probe keeps its kind and
/// its answer together; `void` answers as the empty text.
const SETUP: &str = r#"function dateProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function dateFields(d) {
    return d.getYear() + "," + d.getMonth() + "," + d.getDate() + "," + d.getDay() + "," + d.getHours() + "," + d.getMinutes() + "," + d.getSeconds() + "," + d.getTime();
}
function dateBase() {
    return new Date(2004, 4, 3, 11, 22, 33);
}
function dateSetYear(value) {
    var d = dateBase();
    d.setYear(value);
    return dateFields(d);
}
function dateSetMonth(value) {
    var d = dateBase();
    d.setMonth(value);
    return dateFields(d);
}
function dateSetDate(value) {
    var d = dateBase();
    d.setDate(value);
    return dateFields(d);
}
function dateSetHours(value) {
    var d = dateBase();
    d.setHours(value);
    return dateFields(d);
}
function dateSetMinutes(value) {
    var d = dateBase();
    d.setMinutes(value);
    return dateFields(d);
}
function dateSetSeconds(value) {
    var d = dateBase();
    d.setSeconds(value);
    return dateFields(d);
}
function dateSetYearResult(value) {
    var d = dateBase();
    return d.setYear(value);
}
function dateSetTimeResult(value) {
    var d = dateBase();
    return d.setTime(value);
}
function dateSetTime(ms) {
    var d = dateBase();
    d.setTime(ms);
    return dateFields(d);
}
function dateParseFields(text) {
    var d = new Date(2000, 0, 1, 0, 0, 0);
    d.parse(text);
    return dateFields(d);
}
function dateParseNew(text) {
    return dateFields(new Date(text));
}
"#;

/// The probes: the id the check names the rule by, and the expression.
const PROBES: &[(&str, &str)] = &[
    // the class surface: the members it publishes, and the ones it does not
    ("class_typeof", "dateProbe(typeof Date)"),
    ("now_typeof", "dateProbe(typeof (new Date()))"),
    ("absent_now", "dateProbe(typeof Date.now)"),
    ("absent_utc", "dateProbe(typeof Date.UTC)"),
    (
        "absent_to_string",
        "dateProbe(typeof (dateBase().toString))",
    ),
    ("absent_value_of", "dateProbe(typeof (dateBase().valueOf))"),
    (
        "absent_get_full_year",
        "dateProbe(typeof (dateBase().getFullYear))",
    ),
    (
        "absent_get_utc_hours",
        "dateProbe(typeof (dateBase().getUTCHours))",
    ),
    (
        "absent_get_milliseconds",
        "dateProbe(typeof (dateBase().getMilliseconds))",
    ),
    ("member_get_time", "dateProbe(typeof (dateBase().getTime))"),
    ("member_parse", "dateProbe(typeof (dateBase().parse))"),
    ("member_set_year", "dateProbe(typeof (dateBase().setYear))"),
    (
        "class_parse_raises",
        r#"dateProbe(Date.parse("3 May 2004 11:22:33 GMT"))"#,
    ),
    ("class_set_year_raises", "dateProbe(Date.setYear(1999))"),
    (
        "class_timezone_offset",
        "dateProbe(Date.getTimezoneOffset())",
    ),
    (
        "instance_timezone_offset",
        "dateProbe(dateBase().getTimezoneOffset())",
    ),
    // the constructor and the getters
    ("built_fields", "dateProbe(dateFields(dateBase()))"),
    ("built_one_arg", "dateProbe(dateFields(new Date(2020)))"),
    ("built_two_args", "dateProbe(dateFields(new Date(2020, 5)))"),
    (
        "built_three_args",
        "dateProbe(dateFields(new Date(2020, 5, 15)))",
    ),
    (
        "built_four_args",
        "dateProbe(dateFields(new Date(2020, 5, 15, 12)))",
    ),
    (
        "built_five_args",
        "dateProbe(dateFields(new Date(2020, 5, 15, 12, 30)))",
    ),
    (
        "built_six_args",
        "dateProbe(dateFields(new Date(2020, 5, 15, 12, 30, 45)))",
    ),
    (
        "built_month_12",
        "dateProbe(dateFields(new Date(2020, 12, 1, 0, 0, 0)))",
    ),
    (
        "built_month_negative",
        "dateProbe(dateFields(new Date(2020, -1, 1, 0, 0, 0)))",
    ),
    (
        "built_month_24",
        "dateProbe(dateFields(new Date(2020, 24, 1, 0, 0, 0)))",
    ),
    (
        "built_day_32",
        "dateProbe(dateFields(new Date(2020, 0, 32, 0, 0, 0)))",
    ),
    (
        "built_day_0",
        "dateProbe(dateFields(new Date(2020, 0, 0, 0, 0, 0)))",
    ),
    (
        "built_day_negative",
        "dateProbe(dateFields(new Date(2020, 0, -1, 0, 0, 0)))",
    ),
    (
        "built_hour_25",
        "dateProbe(dateFields(new Date(2020, 0, 1, 25, 0, 0)))",
    ),
    (
        "built_hour_negative",
        "dateProbe(dateFields(new Date(2020, 0, 1, -1, 0, 0)))",
    ),
    (
        "built_minute_90",
        "dateProbe(dateFields(new Date(2020, 0, 1, 0, 90, 90)))",
    ),
    (
        "built_second_negative",
        "dateProbe(dateFields(new Date(2020, 0, 1, 0, 0, -1)))",
    ),
    (
        "built_leap_day",
        "dateProbe(dateFields(new Date(2020, 1, 29, 12, 0, 0)))",
    ),
    (
        "built_common_day",
        "dateProbe(dateFields(new Date(2019, 1, 29, 12, 0, 0)))",
    ),
    (
        "built_year_0",
        "dateProbe(dateFields(new Date(0, 0, 1, 0, 0, 0)))",
    ),
    (
        "built_year_50",
        "dateProbe(dateFields(new Date(50, 0, 1, 0, 0, 0)))",
    ),
    (
        "built_year_1900",
        "dateProbe(dateFields(new Date(1900, 0, 1, 0, 0, 0)))",
    ),
    (
        "built_year_100000",
        "dateProbe(dateFields(new Date(100000, 0, 1, 0, 0, 0)))",
    ),
    (
        "built_year_wrapped",
        "dateProbe(dateFields(new Date(4294969296, 0, 1, 0, 0, 0)))",
    ),
    (
        "built_real_fields",
        "dateProbe(dateFields(new Date(2020.9, 5.9, 15.9, 12.9, 30.9, 45.9)))",
    ),
    (
        "built_void_day",
        "dateProbe(dateFields(new Date(2020, void, void)))",
    ),
    (
        "built_epoch_before",
        "dateProbe(dateFields(new Date(1970, 0, 1, 8, 59, 58)))",
    ),
    (
        "built_epoch_exact",
        "dateProbe(dateFields(new Date(1970, 0, 1, 8, 59, 59)))",
    ),
    (
        "built_epoch_after",
        "dateProbe(dateFields(new Date(1970, 0, 1, 9, 0, 0)))",
    ),
    // the millisecond pair
    ("set_time_result", r#"dateProbe(dateSetTimeResult(1500))"#),
    ("set_time_positive", "dateProbe(dateSetTime(1500))"),
    ("set_time_negative", "dateProbe(dateSetTime(-1500))"),
    ("set_time_exact", "dateProbe(dateSetTime(1700000000123))"),
    ("set_time_real", "dateProbe(dateSetTime(1500.9))"),
    ("set_time_zero", "dateProbe(dateSetTime(0))"),
    ("set_time_huge", "dateProbe(dateSetTime(1e30))"),
    // the setters, and the field each one replaces
    ("set_year", r#"dateProbe(dateSetYear(1999))"#),
    ("set_year_two_digits", r#"dateProbe(dateSetYear(99))"#),
    ("set_year_zero", r#"dateProbe(dateSetYear(0))"#),
    ("set_year_negative", r#"dateProbe(dateSetYear(-1))"#),
    ("set_year_real", r#"dateProbe(dateSetYear(1999.9))"#),
    ("set_year_result", r#"dateProbe(dateSetYearResult(1999))"#),
    ("set_month", r#"dateProbe(dateSetMonth(0))"#),
    ("set_month_12", r#"dateProbe(dateSetMonth(12))"#),
    ("set_month_negative", r#"dateProbe(dateSetMonth(-1))"#),
    ("set_date", r#"dateProbe(dateSetDate(15))"#),
    ("set_date_zero", r#"dateProbe(dateSetDate(0))"#),
    ("set_date_32", r#"dateProbe(dateSetDate(32))"#),
    ("set_hours", r#"dateProbe(dateSetHours(0))"#),
    ("set_hours_25", r#"dateProbe(dateSetHours(25))"#),
    ("set_minutes", r#"dateProbe(dateSetMinutes(0))"#),
    ("set_minutes_90", r#"dateProbe(dateSetMinutes(90))"#),
    ("set_seconds", r#"dateProbe(dateSetSeconds(0))"#),
    ("set_seconds_90", r#"dateProbe(dateSetSeconds(90))"#),
    ("set_seconds_negative", r#"dateProbe(dateSetSeconds(-1))"#),
    ("set_year_missing", "dateProbe(dateBase().setYear())"),
    ("set_month_missing", "dateProbe(dateBase().setMonth())"),
    ("set_date_missing", "dateProbe(dateBase().setDate())"),
    ("set_hours_missing", "dateProbe(dateBase().setHours())"),
    ("set_minutes_missing", "dateProbe(dateBase().setMinutes())"),
    ("set_seconds_missing", "dateProbe(dateBase().setSeconds())"),
    ("set_time_missing", "dateProbe(dateBase().setTime())"),
    // the grammar's forms
    (
        "parse_alt1",
        r#"dateProbe(dateParseFields("Sun, 3 May 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt1_no_comma",
        r#"dateProbe(dateParseFields("Sun 3 May 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt1_no_wday",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt2",
        r#"dateProbe(dateParseFields("Sun, 3-May 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt3",
        r#"dateProbe(dateParseFields("Sun, 3-May-2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt4",
        r#"dateProbe(dateParseFields("Sun, May 3 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt5",
        r#"dateProbe(dateParseFields("Sun, May-3 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt6",
        r#"dateProbe(dateParseFields("Sun, May-3-2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_alt7",
        r#"dateProbe(dateParseFields("Sun, 3 May 11:22:33 2004 GMT +900"))"#,
    ),
    (
        "parse_alt8",
        r#"dateProbe(dateParseFields("Sun, 3-May 11:22:33 2004 GMT +900"))"#,
    ),
    (
        "parse_alt9",
        r#"dateProbe(dateParseFields("Sun, May 3 11:22:33 2004 GMT +900"))"#,
    ),
    (
        "parse_alt10",
        r#"dateProbe(dateParseFields("Sun, May-3 11:22:33 2004 GMT +900"))"#,
    ),
    (
        "parse_ymd_slash",
        r#"dateProbe(dateParseFields("2004/03/03 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_ymd_dash",
        r#"dateProbe(dateParseFields("2004-03-03 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_ymd_mixed",
        r#"dateProbe(dateParseFields("2004/03-03 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_ymd_month_13",
        r#"dateProbe(dateParseFields("2004/13/03 11:22:33 GMT"))"#,
    ),
    (
        "parse_ymd_day_0",
        r#"dateProbe(dateParseFields("2004/03/00 11:22:33 GMT"))"#,
    ),
    (
        "parse_ymd_day_32",
        r#"dateProbe(dateParseFields("2004/03/32 11:22:33 GMT"))"#,
    ),
    (
        "parse_hm_only",
        r#"dateProbe(dateParseFields("3 May 2004 11:22 GMT +900"))"#,
    ),
    (
        "parse_fraction",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33.1234 GMT +900"))"#,
    ),
    (
        "parse_am_before",
        r#"dateProbe(dateParseFields("3 May 2004 AM 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_pm_before",
        r#"dateProbe(dateParseFields("3 May 2004 PM 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_pm_after",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 PM GMT +900"))"#,
    ),
    (
        "parse_am_after",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 AM GMT +900"))"#,
    ),
    (
        "parse_pm_noon",
        r#"dateProbe(dateParseFields("3 May 2004 12:00:00 PM GMT +900"))"#,
    ),
    (
        "parse_am_midnight",
        r#"dateProbe(dateParseFields("3 May 2004 12:00:00 AM GMT +900"))"#,
    ),
    (
        "parse_pm_no_space",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33PM GMT +900"))"#,
    ),
    (
        "parse_tz_jst",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 JST"))"#,
    ),
    (
        "parse_tz_lower",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 jst"))"#,
    ),
    (
        "parse_tz_ut",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 UT"))"#,
    ),
    (
        "parse_tz_est",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 EST"))"#,
    ),
    (
        "parse_tz_gmt_negative",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT -500"))"#,
    ),
    (
        "parse_tz_offset_only",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 +900"))"#,
    ),
    (
        "parse_tz_offset_negative",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 -500"))"#,
    ),
    (
        "parse_tz_z",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 Z"))"#,
    ),
    (
        "parse_tz_desc",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT +900 (JST)"))"#,
    ),
    (
        "parse_tz_desc_only",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 (JST)"))"#,
    ),
    (
        "parse_tz_glued",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT+900"))"#,
    ),
    (
        "parse_no_tz",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33"))"#,
    ),
    (
        "parse_no_spaces",
        r#"dateProbe(dateParseFields("3May2004 11:22:33 GMT+900"))"#,
    ),
    (
        "parse_whitespace",
        "dateProbe(dateParseFields(\"\\t 3 \\n May 2004 11:22:33 GMT \\r\"))",
    ),
    (
        "parse_two_digit_04",
        r#"dateProbe(dateParseFields("3 May 04 11:22:33 GMT"))"#,
    ),
    (
        "parse_two_digit_50",
        r#"dateProbe(dateParseFields("3 May 50 11:22:33 GMT"))"#,
    ),
    (
        "parse_two_digit_51",
        r#"dateProbe(dateParseFields("3 May 51 11:22:33 GMT"))"#,
    ),
    (
        "parse_two_digit_99",
        r#"dateProbe(dateParseFields("3 May 99 11:22:33 GMT"))"#,
    ),
    (
        "parse_two_digit_00",
        r#"dateProbe(dateParseFields("3 May 00 11:22:33 GMT"))"#,
    ),
    (
        "parse_ymd_two_digit",
        r#"dateProbe(dateParseFields("04/03/03 11:22:33 GMT"))"#,
    ),
    (
        "parse_month_full",
        r#"dateProbe(dateParseFields("3 September 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_month_dotted",
        r#"dateProbe(dateParseFields("3 Sep. 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_month_ju",
        r#"dateProbe(dateParseFields("3 ju 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_month_jul",
        r#"dateProbe(dateParseFields("3 jul 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_month_upper",
        r#"dateProbe(dateParseFields("3 MAY 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_wday_dotted",
        r#"dateProbe(dateParseFields("Mon., 3 May 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_wday_wrong",
        r#"dateProbe(dateParseFields("Sun, 3 May 2004 11:22:33 GMT"))"#,
    ),
    (
        "parse_year_wrap",
        r#"dateProbe(dateParseFields("3 May 4294969296 11:22:33 GMT"))"#,
    ),
    (
        "parse_new_string",
        r#"dateProbe(dateParseNew("3 May 2004 11:22:33 GMT +900"))"#,
    ),
    (
        "parse_epoch_before",
        r#"dateProbe(dateParseFields("1 Jan 1970 08:59:58"))"#,
    ),
    (
        "parse_epoch_exact",
        r#"dateProbe(dateParseFields("1 Jan 1970 08:59:59"))"#,
    ),
    (
        "parse_epoch_after",
        r#"dateProbe(dateParseFields("1 Jan 1970 09:00:00"))"#,
    ),
    // the grammar's refusals
    ("refuse_spaces", r#"dateProbe(dateParseFields("   "))"#),
    (
        "refuse_time_only",
        r#"dateProbe(dateParseFields("11:22:33"))"#,
    ),
    (
        "refuse_date_only",
        r#"dateProbe(dateParseFields("3 May 2004"))"#,
    ),
    (
        "refuse_no_year",
        r#"dateProbe(dateParseFields("3 May 11:22:33 GMT"))"#,
    ),
    (
        "refuse_no_day",
        r#"dateProbe(dateParseFields("May 2004 11:22:33 GMT"))"#,
    ),
    (
        "refuse_two_ymd",
        r#"dateProbe(dateParseFields("2004/03 11:22:33 GMT"))"#,
    ),
    (
        "refuse_bad_tz",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 XYZ"))"#,
    ),
    (
        "refuse_trailing",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT +900 junk"))"#,
    ),
    (
        "refuse_fraction_without_seconds",
        r#"dateProbe(dateParseFields("3 May 2004 11:22.5 GMT"))"#,
    ),
    (
        "refuse_dot_no_digits",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33. GMT"))"#,
    ),
    (
        "refuse_unclosed_paren",
        r#"dateProbe(dateParseFields("3 May 2004 11:22:33 GMT (JST"))"#,
    ),
    (
        "refuse_double_pm",
        r#"dateProbe(dateParseFields("3 May 2004 AM 11:22:33 PM GMT"))"#,
    ),
    (
        "refuse_janx",
        r#"dateProbe(dateParseFields("3 Janx 2004 11:22:33 GMT"))"#,
    ),
    (
        "refuse_sundayx",
        r#"dateProbe(dateParseFields("Sundayx, 3 May 2004 11:22:33 GMT"))"#,
    ),
    (
        "refuse_wide",
        "dateProbe(dateParseFields(\"3 May\u{3002}2004 11:22:33 GMT\"))",
    ),
    (
        "refuse_negative_year",
        r#"dateProbe(dateParseFields("-3 May 2004 11:22:33 GMT"))"#,
    ),
    (
        "refuse_second_missing",
        r#"dateProbe(dateParseFields("3 May 2004 11:22: GMT"))"#,
    ),
    (
        "refuse_minute_missing",
        r#"dateProbe(dateParseFields("3 May 2004 11: GMT"))"#,
    ),
    (
        "refuse_may_may",
        r#"dateProbe(dateParseFields("May May 2004 11:22:33 GMT"))"#,
    ),
    (
        "refuse_no_time",
        r#"dateProbe(dateParseFields("3 May 2004 GMT"))"#,
    ),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "date_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    check_zone(&engine);
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let answer = answer(&engine, expression, probe);
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

/// Refuses to record unless the process runs at the fixed recorded zone.
///
/// A record taken where the offset changes over the year would carry answers
/// that no fixed offset reproduces: the winter and summer noon fields must
/// both read back as noon, and the offset must be the recorded one.
fn check_zone(engine: &Tjs2Engine) {
    let offset = integer(engine, "dateBase().getTimezoneOffset()", "zone offset");
    let winter = integer(
        engine,
        "(new Date(2020, 0, 15, 12, 0, 0)).getHours()",
        "winter hour",
    );
    let summer = integer(
        engine,
        "(new Date(2020, 6, 15, 12, 0, 0)).getHours()",
        "summer hour",
    );
    if offset != RECORDED_MINUTES_WEST || winter != 12 || summer != 12 {
        fail(format_args!(
            "the process zone is not fixed at nine hours east (offset {offset}, winter {winter}, \
             summer {summer}); record with `TZ=JST-9 cargo run -p tjs --example date_oracle`"
        ));
    }
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/date_members.txt")
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
    match engine.eval(call, "date_members") {
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

/// The integer one expression answers, for the zone check.
fn integer(engine: &Tjs2Engine, expression: &str, what: &str) -> i64 {
    match engine.eval(expression, "date_members") {
        Ok(TjsValue::Integer(value)) => value,
        Ok(other) => fail(format_args!(
            "{what}: the answer is not an integer: {other:?}"
        )),
        Err(error) => fail(format_args!("{what}: the engine raised: {error}")),
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
    eprintln!("date_oracle: {message}");
    std::process::exit(1)
}
