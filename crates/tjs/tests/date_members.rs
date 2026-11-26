//! The calendar and the date grammar against the engine's recorded answers.
//!
//! `tests/fixtures/date_members.txt` holds what the engine answered for a
//! date's fields, its epoch, its setters and its text grammar;
//! `crates/tjs/examples/date_oracle.rs` records it. The check below replays
//! the same probes through the calendar (`tjs::vm::runtime::natives::date`)
//! and the grammar (`tjs::vm::runtime::natives::date_parse`) and compares the
//! kind and the text of every answer.
//!
//! The record carries its zone in `instance_timezone_offset`: the fields a
//! script reads are local, so a record taken at one offset is replayed at that
//! offset, never at the machine's. The recorder refuses to write unless the
//! process ran at nine hours east of UTC, and the check says so when the
//! record is not that one.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::natives::date::{self, DateError, Fields};
use tjs::vm::runtime::natives::date_parse;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "class_typeof",
    "now_typeof",
    "absent_now",
    "absent_utc",
    "absent_to_string",
    "absent_value_of",
    "absent_get_full_year",
    "absent_get_utc_hours",
    "absent_get_milliseconds",
    "member_get_time",
    "member_parse",
    "member_set_year",
    "class_parse_raises",
    "class_set_year_raises",
    "class_timezone_offset",
    "instance_timezone_offset",
    "built_fields",
    "built_one_arg",
    "built_two_args",
    "built_three_args",
    "built_four_args",
    "built_five_args",
    "built_six_args",
    "built_month_12",
    "built_month_negative",
    "built_month_24",
    "built_day_32",
    "built_day_0",
    "built_day_negative",
    "built_hour_25",
    "built_hour_negative",
    "built_minute_90",
    "built_second_negative",
    "built_leap_day",
    "built_common_day",
    "built_year_0",
    "built_year_50",
    "built_year_1900",
    "built_year_100000",
    "built_year_wrapped",
    "built_real_fields",
    "built_void_day",
    "built_epoch_before",
    "built_epoch_exact",
    "built_epoch_after",
    "set_time_result",
    "set_time_positive",
    "set_time_negative",
    "set_time_exact",
    "set_time_real",
    "set_time_zero",
    "set_time_huge",
    "set_year",
    "set_year_two_digits",
    "set_year_zero",
    "set_year_negative",
    "set_year_real",
    "set_year_result",
    "set_month",
    "set_month_12",
    "set_month_negative",
    "set_date",
    "set_date_zero",
    "set_date_32",
    "set_hours",
    "set_hours_25",
    "set_minutes",
    "set_minutes_90",
    "set_seconds",
    "set_seconds_90",
    "set_seconds_negative",
    "set_year_missing",
    "set_month_missing",
    "set_date_missing",
    "set_hours_missing",
    "set_minutes_missing",
    "set_seconds_missing",
    "set_time_missing",
    "parse_alt1",
    "parse_alt1_no_comma",
    "parse_alt1_no_wday",
    "parse_alt2",
    "parse_alt3",
    "parse_alt4",
    "parse_alt5",
    "parse_alt6",
    "parse_alt7",
    "parse_alt8",
    "parse_alt9",
    "parse_alt10",
    "parse_ymd_slash",
    "parse_ymd_dash",
    "parse_ymd_mixed",
    "parse_ymd_month_13",
    "parse_ymd_day_0",
    "parse_ymd_day_32",
    "parse_hm_only",
    "parse_fraction",
    "parse_am_before",
    "parse_pm_before",
    "parse_pm_after",
    "parse_am_after",
    "parse_pm_noon",
    "parse_am_midnight",
    "parse_pm_no_space",
    "parse_tz_jst",
    "parse_tz_lower",
    "parse_tz_ut",
    "parse_tz_est",
    "parse_tz_gmt_negative",
    "parse_tz_offset_only",
    "parse_tz_offset_negative",
    "parse_tz_z",
    "parse_tz_desc",
    "parse_tz_desc_only",
    "parse_tz_glued",
    "parse_no_tz",
    "parse_no_spaces",
    "parse_whitespace",
    "parse_two_digit_04",
    "parse_two_digit_50",
    "parse_two_digit_51",
    "parse_two_digit_99",
    "parse_two_digit_00",
    "parse_ymd_two_digit",
    "parse_month_full",
    "parse_month_dotted",
    "parse_month_ju",
    "parse_month_jul",
    "parse_month_upper",
    "parse_wday_dotted",
    "parse_wday_wrong",
    "parse_year_wrap",
    "parse_new_string",
    "parse_epoch_before",
    "parse_epoch_exact",
    "parse_epoch_after",
    "refuse_spaces",
    "refuse_time_only",
    "refuse_date_only",
    "refuse_no_year",
    "refuse_no_day",
    "refuse_two_ymd",
    "refuse_bad_tz",
    "refuse_trailing",
    "refuse_fraction_without_seconds",
    "refuse_dot_no_digits",
    "refuse_unclosed_paren",
    "refuse_double_pm",
    "refuse_janx",
    "refuse_sundayx",
    "refuse_wide",
    "refuse_negative_year",
    "refuse_second_missing",
    "refuse_minute_missing",
    "refuse_may_may",
    "refuse_no_time",
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text.
    text: String,
}

#[test]
fn the_calendar_and_the_grammar_reproduce_the_recorded_date_answers() {
    let records = recorded_answers();
    let seen: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `TZ=JST-9 cargo run -p tjs --example date_oracle`"
    );
    let zone = records
        .iter()
        .find(|record| record.probe == "instance_timezone_offset")
        .expect("the record holds the zone");
    assert_eq!(zone.kind, "Integer", "the zone is an integer");
    let minutes: i64 = match zone.text.parse() {
        Ok(minutes) => minutes,
        Err(error) => panic!("the recorded zone is not an integer: {error}"),
    };
    assert_eq!(
        minutes, -540,
        "the record was not taken at nine hours east of UTC; record it again with \
         `TZ=JST-9 cargo run -p tjs --example date_oracle`"
    );
    let offset = -minutes * 60;
    for record in &records {
        let (kind, text) = answer(&record.probe, offset);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
    }
}

/// The answer one probe must give.
fn answer(probe: &str, offset: i64) -> (String, String) {
    // a setter answers nothing, and the record says so
    if matches!(probe, "set_time_result" | "set_year_result") {
        return (String::from("void"), String::new());
    }
    if matches!(probe, "class_timezone_offset" | "instance_timezone_offset") {
        return match date::timezone_minutes(offset) {
            Ok(minutes) => (String::from("Integer"), minutes.to_string()),
            Err(_) => raised(),
        };
    }
    match expected(probe, offset) {
        Ok(text) => (String::from("String"), text),
        Err(_) => raised(),
    }
}

/// The raised answer: the kind and its empty text.
fn raised() -> (String, String) {
    (String::from("!raise"), String::new())
}

/// The text a probe's expression answers, or the refusal.
fn expected(probe: &str, offset: i64) -> Result<String, DateError> {
    match probe {
        // The class surface is the class object's; the record pins it so the
        // calendar never invents a member the engine does not publish.
        "class_typeof" | "now_typeof" => Ok(String::from("Object")),
        "absent_now"
        | "absent_utc"
        | "absent_to_string"
        | "absent_value_of"
        | "absent_get_full_year"
        | "absent_get_utc_hours"
        | "absent_get_milliseconds" => Ok(String::from("undefined")),
        // a method reference is an object (a closure), not a function value
        "member_get_time" | "member_parse" | "member_set_year" => Ok(String::from("Object")),
        "class_parse_raises" | "class_set_year_raises" => Err(DateError::CannotParse),
        "set_year_missing"
        | "set_month_missing"
        | "set_date_missing"
        | "set_hours_missing"
        | "set_minutes_missing"
        | "set_seconds_missing"
        | "set_time_missing" => Err(DateError::CannotParse),
        // The constructor and its getters.
        "built_fields" => construct(&[2004, 4, 3, 11, 22, 33], offset),
        "built_one_arg" => construct(&[2020], offset),
        "built_two_args" => construct(&[2020, 5], offset),
        "built_three_args" => construct(&[2020, 5, 15], offset),
        "built_four_args" => construct(&[2020, 5, 15, 12], offset),
        "built_five_args" => construct(&[2020, 5, 15, 12, 30], offset),
        "built_six_args" => construct(&[2020, 5, 15, 12, 30, 45], offset),
        "built_month_12" => construct(&[2020, 12, 1, 0, 0, 0], offset),
        "built_month_negative" => construct(&[2020, -1, 1, 0, 0, 0], offset),
        "built_month_24" => construct(&[2020, 24, 1, 0, 0, 0], offset),
        "built_day_32" => construct(&[2020, 0, 32, 0, 0, 0], offset),
        "built_day_0" => construct(&[2020, 0, 0, 0, 0, 0], offset),
        "built_day_negative" => construct(&[2020, 0, -1, 0, 0, 0], offset),
        "built_hour_25" => construct(&[2020, 0, 1, 25, 0, 0], offset),
        "built_hour_negative" => construct(&[2020, 0, 1, -1, 0, 0], offset),
        "built_minute_90" => construct(&[2020, 0, 1, 0, 90, 90], offset),
        "built_second_negative" => construct(&[2020, 0, 1, 0, 0, -1], offset),
        "built_leap_day" => construct(&[2020, 1, 29, 12, 0, 0], offset),
        "built_common_day" => construct(&[2019, 1, 29, 12, 0, 0], offset),
        "built_year_0" => construct(&[0, 0, 1, 0, 0, 0], offset),
        "built_year_50" => construct(&[50, 0, 1, 0, 0, 0], offset),
        "built_year_1900" => construct(&[1900, 0, 1, 0, 0, 0], offset),
        "built_year_100000" => construct(&[100000, 0, 1, 0, 0, 0], offset),
        "built_year_wrapped" => construct(&[4_294_969_296, 0, 1, 0, 0, 0], offset),
        "built_real_fields" => construct(&[2020, 5, 15, 12, 30, 45], offset),
        "built_void_day" => construct(&[2020], offset),
        "built_epoch_before" => construct(&[1970, 0, 1, 8, 59, 58], offset),
        "built_epoch_exact" => construct(&[1970, 0, 1, 8, 59, 59], offset),
        "built_epoch_after" => construct(&[1970, 0, 1, 9, 0, 0], offset),
        // The millisecond pair.
        "set_time_positive" => set_time(1_500, offset),
        "set_time_negative" => set_time(-1_500, offset),
        "set_time_exact" => set_time(1_700_000_000_123, offset),
        "set_time_real" => set_time(1_500, offset),
        "set_time_zero" => set_time(0, offset),
        // an argument outside the integer range converts to the lowest
        // integer, and the millisecond division then truncates toward zero
        "set_time_huge" => set_time(i64::MIN, offset),
        // The setters.
        "set_year" => set("setYear", 1999, offset),
        "set_year_two_digits" => set("setYear", 99, offset),
        "set_year_zero" => set("setYear", 0, offset),
        "set_year_negative" => set("setYear", -1, offset),
        "set_year_real" => set("setYear", 1999, offset),
        "set_month" => set("setMonth", 0, offset),
        "set_month_12" => set("setMonth", 12, offset),
        "set_month_negative" => set("setMonth", -1, offset),
        "set_date" => set("setDate", 15, offset),
        "set_date_zero" => set("setDate", 0, offset),
        "set_date_32" => set("setDate", 32, offset),
        "set_hours" => set("setHours", 0, offset),
        "set_hours_25" => set("setHours", 25, offset),
        "set_minutes" => set("setMinutes", 0, offset),
        "set_minutes_90" => set("setMinutes", 90, offset),
        "set_seconds" => set("setSeconds", 0, offset),
        "set_seconds_90" => set("setSeconds", 90, offset),
        "set_seconds_negative" => set("setSeconds", -1, offset),
        // The grammar's forms.
        "parse_alt1" => parsed("Sun, 3 May 2004 11:22:33 GMT +900", offset),
        "parse_alt1_no_comma" => parsed("Sun 3 May 2004 11:22:33 GMT +900", offset),
        "parse_alt1_no_wday" => parsed("3 May 2004 11:22:33 GMT +900", offset),
        "parse_alt2" => parsed("Sun, 3-May 2004 11:22:33 GMT +900", offset),
        "parse_alt3" => parsed("Sun, 3-May-2004 11:22:33 GMT +900", offset),
        "parse_alt4" => parsed("Sun, May 3 2004 11:22:33 GMT +900", offset),
        "parse_alt5" => parsed("Sun, May-3 2004 11:22:33 GMT +900", offset),
        "parse_alt6" => parsed("Sun, May-3-2004 11:22:33 GMT +900", offset),
        "parse_alt7" => parsed("Sun, 3 May 11:22:33 2004 GMT +900", offset),
        "parse_alt8" => parsed("Sun, 3-May 11:22:33 2004 GMT +900", offset),
        "parse_alt9" => parsed("Sun, May 3 11:22:33 2004 GMT +900", offset),
        "parse_alt10" => parsed("Sun, May-3 11:22:33 2004 GMT +900", offset),
        "parse_ymd_slash" => parsed("2004/03/03 11:22:33 GMT +900", offset),
        "parse_ymd_dash" => parsed("2004-03-03 11:22:33 GMT +900", offset),
        "parse_ymd_mixed" => parsed("2004/03-03 11:22:33 GMT +900", offset),
        "parse_ymd_month_13" => parsed("2004/13/03 11:22:33 GMT", offset),
        "parse_ymd_day_0" => parsed("2004/03/00 11:22:33 GMT", offset),
        "parse_ymd_day_32" => parsed("2004/03/32 11:22:33 GMT", offset),
        "parse_hm_only" => parsed("3 May 2004 11:22 GMT +900", offset),
        "parse_fraction" => parsed("3 May 2004 11:22:33.1234 GMT +900", offset),
        "parse_am_before" => parsed("3 May 2004 AM 11:22:33 GMT +900", offset),
        "parse_pm_before" => parsed("3 May 2004 PM 11:22:33 GMT +900", offset),
        "parse_pm_after" => parsed("3 May 2004 11:22:33 PM GMT +900", offset),
        "parse_am_after" => parsed("3 May 2004 11:22:33 AM GMT +900", offset),
        "parse_pm_noon" => parsed("3 May 2004 12:00:00 PM GMT +900", offset),
        "parse_am_midnight" => parsed("3 May 2004 12:00:00 AM GMT +900", offset),
        "parse_pm_no_space" => parsed("3 May 2004 11:22:33PM GMT +900", offset),
        "parse_tz_jst" => parsed("3 May 2004 11:22:33 JST", offset),
        "parse_tz_lower" => parsed("3 May 2004 11:22:33 jst", offset),
        "parse_tz_ut" => parsed("3 May 2004 11:22:33 UT", offset),
        "parse_tz_est" => parsed("3 May 2004 11:22:33 EST", offset),
        "parse_tz_gmt_negative" => parsed("3 May 2004 11:22:33 GMT -500", offset),
        "parse_tz_offset_only" => parsed("3 May 2004 11:22:33 +900", offset),
        "parse_tz_offset_negative" => parsed("3 May 2004 11:22:33 -500", offset),
        "parse_tz_z" => parsed("3 May 2004 11:22:33 Z", offset),
        "parse_tz_desc" => parsed("3 May 2004 11:22:33 GMT +900 (JST)", offset),
        "parse_tz_desc_only" => parsed("3 May 2004 11:22:33 (JST)", offset),
        "parse_tz_glued" => parsed("3 May 2004 11:22:33 GMT+900", offset),
        "parse_no_tz" => parsed("3 May 2004 11:22:33", offset),
        "parse_no_spaces" => parsed("3May2004 11:22:33 GMT+900", offset),
        "parse_whitespace" => parsed("\t 3 \n May 2004 11:22:33 GMT \r", offset),
        "parse_two_digit_04" => parsed("3 May 04 11:22:33 GMT", offset),
        "parse_two_digit_50" => parsed("3 May 50 11:22:33 GMT", offset),
        "parse_two_digit_51" => parsed("3 May 51 11:22:33 GMT", offset),
        "parse_two_digit_99" => parsed("3 May 99 11:22:33 GMT", offset),
        "parse_two_digit_00" => parsed("3 May 00 11:22:33 GMT", offset),
        "parse_ymd_two_digit" => parsed("04/03/03 11:22:33 GMT", offset),
        "parse_month_full" => parsed("3 September 2004 11:22:33 GMT", offset),
        "parse_month_dotted" => parsed("3 Sep. 2004 11:22:33 GMT", offset),
        "parse_month_ju" => parsed("3 ju 2004 11:22:33 GMT", offset),
        "parse_month_jul" => parsed("3 jul 2004 11:22:33 GMT", offset),
        "parse_month_upper" => parsed("3 MAY 2004 11:22:33 GMT", offset),
        "parse_wday_dotted" => parsed("Mon., 3 May 2004 11:22:33 GMT", offset),
        "parse_wday_wrong" => parsed("Sun, 3 May 2004 11:22:33 GMT", offset),
        "parse_year_wrap" => parsed("3 May 4294969296 11:22:33 GMT", offset),
        "parse_new_string" => parsed("3 May 2004 11:22:33 GMT +900", offset),
        "parse_epoch_before" => parsed("1 Jan 1970 08:59:58", offset),
        "parse_epoch_exact" => parsed("1 Jan 1970 08:59:59", offset),
        "parse_epoch_after" => parsed("1 Jan 1970 09:00:00", offset),
        // The grammar's refusals.
        "refuse_spaces" => parsed("   ", offset),
        "refuse_time_only" => parsed("11:22:33", offset),
        "refuse_date_only" => parsed("3 May 2004", offset),
        "refuse_no_year" => parsed("3 May 11:22:33 GMT", offset),
        "refuse_no_day" => parsed("May 2004 11:22:33 GMT", offset),
        "refuse_two_ymd" => parsed("2004/03 11:22:33 GMT", offset),
        "refuse_bad_tz" => parsed("3 May 2004 11:22:33 XYZ", offset),
        "refuse_trailing" => parsed("3 May 2004 11:22:33 GMT +900 junk", offset),
        "refuse_fraction_without_seconds" => parsed("3 May 2004 11:22.5 GMT", offset),
        "refuse_dot_no_digits" => parsed("3 May 2004 11:22:33. GMT", offset),
        "refuse_unclosed_paren" => parsed("3 May 2004 11:22:33 GMT (JST", offset),
        "refuse_double_pm" => parsed("3 May 2004 AM 11:22:33 PM GMT", offset),
        "refuse_janx" => parsed("3 Janx 2004 11:22:33 GMT", offset),
        "refuse_sundayx" => parsed("Sundayx, 3 May 2004 11:22:33 GMT", offset),
        "refuse_wide" => parsed("3 May\u{3002}2004 11:22:33 GMT", offset),
        "refuse_negative_year" => parsed("-3 May 2004 11:22:33 GMT", offset),
        "refuse_second_missing" => parsed("3 May 2004 11:22: GMT", offset),
        "refuse_minute_missing" => parsed("3 May 2004 11: GMT", offset),
        "refuse_may_may" => parsed("May May 2004 11:22:33 GMT", offset),
        "refuse_no_time" => parsed("3 May 2004 GMT", offset),
        other => panic!("the check has no answer for the recorded probe: {other}"),
    }
}

/// The fields and epoch a date is, as the getters write them.
fn text(seconds: i64, offset: i64) -> Result<String, DateError> {
    let fields = date::local_fields(seconds, offset)?;
    Ok(format!(
        "{},{},{},{},{},{},{},{}",
        fields.year,
        fields.month,
        fields.day,
        fields.weekday,
        fields.hour,
        fields.minute,
        fields.second,
        date::milliseconds(seconds)
    ))
}

/// A field set with an unused weekday.
fn fields(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Fields {
    Fields {
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday: 0,
    }
}

/// The answer of the constructor with the given arguments.
fn construct(arguments: &[i64], offset: i64) -> Result<String, DateError> {
    let year = arguments.first().copied().unwrap_or(0);
    let month = arguments.get(1).copied().unwrap_or(0);
    let day = arguments.get(2).copied().unwrap_or(1);
    let hour = arguments.get(3).copied().unwrap_or(0);
    let minute = arguments.get(4).copied().unwrap_or(0);
    let second = arguments.get(5).copied().unwrap_or(0);
    let seconds = date::local_seconds(&fields(year, month, day, hour, minute, second), offset)?;
    text(seconds, offset)
}

/// The answer of one setter on the base date.
fn set(member: &str, value: i64, offset: i64) -> Result<String, DateError> {
    let mut fields = fields(2004, 4, 3, 11, 22, 33);
    match member {
        "setYear" => fields.year = value,
        "setMonth" => fields.month = value,
        "setDate" => fields.day = value,
        "setHours" => fields.hour = value,
        "setMinutes" => fields.minute = value,
        "setSeconds" => fields.second = value,
        other => panic!("the check has no setter named {other}"),
    }
    let seconds = date::local_seconds(&fields, offset)?;
    text(seconds, offset)
}

/// The answer of the millisecond setter.
fn set_time(milliseconds: i64, offset: i64) -> Result<String, DateError> {
    let seconds = date::seconds_from_milliseconds(milliseconds);
    text(seconds, offset)
}

/// The answer of a text parsed at the recorded zone.
fn parsed(date_text: &str, offset: i64) -> Result<String, DateError> {
    let units: Vec<u16> = date_text.encode_utf16().collect();
    let seconds = date_parse::parse(&units, offset)?;
    text(seconds, offset)
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/date_members.txt");
    let table = fs::read_to_string(&path).expect("read the date member fixture");
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(probe), Some(kind), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            fields.next().is_none(),
            "the fixture line has more than three fields: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            kind: kind.to_owned(),
            text: text.to_owned(),
        });
    }
    assert!(
        !records.is_empty(),
        "the date member fixture records nothing; record it with \
         `TZ=JST-9 cargo run -p tjs --example date_oracle`"
    );
    records
}
