//! The calendar: the fields, the epoch and the zone between them.
//!
//! A date is one signed count of seconds from 1970-01-01 00:00:00 UTC. A
//! script sees it as the fields a wall clock shows: a year, a month counted
//! from zero, a day of the month counted from one, the time of day and the
//! weekday. A zone is the difference between the two, in seconds east of UTC.
//! [`utc_fields`] and [`utc_seconds`] are the pair that does not leave UTC,
//! and [`local_fields`] and [`local_seconds`] are the same pair at a zone's
//! offset.
//!
//! The offset an instant is read at and the offset its fields are written at
//! are separate arguments: a zone whose offset changes over the year names one
//! offset per instant, and the caller selects it. Field writing uses the
//! offset it is given, so a field set is placed at that offset and nowhere
//! else.
//!
//! # Field normalisation
//!
//! A field outside its usual range is carried into the next unit rather than
//! refused: month 12 is the January after the year, day 32 is the first day of
//! the next month, and an hour of 25 is one in the morning of the next day. A
//! negative field carries the other way. The normalisation happens before the
//! calendar names an instant, so `new Date(2020, 12, 1)` is the first day of
//! 2021.
//!
//! # The field width
//!
//! A field enters the calendar as a 32-bit signed integer. A value outside that
//! range keeps its low 32 bits, two's complement, and the calendar reads that
//! value. The narrowing is explicit, so no script value reaches the arithmetic
//! unchecked.
//!
//! # The invalid instant
//!
//! A field set that lands on exactly -1 seconds is refused, and so is one that
//! does not fit the epoch's 64 bits. The class raises its invalid-timestamp
//! error for both; a date that was already stored may still be -1, because
//! only the field-to-epoch direction has the refusal.

/// The seconds in one minute.
const SECONDS_PER_MINUTE: i64 = 60;

/// The seconds in one hour.
const SECONDS_PER_HOUR: i64 = 3_600;

/// The seconds in one day.
const SECONDS_PER_DAY: i64 = 86_400;

/// The days in one 400-year Gregorian era.
const DAYS_PER_ERA: i128 = 146_097;

/// The weekday of 1970-01-01, counted from Sunday as zero.
const EPOCH_WEEKDAY: i64 = 4;

/// The number of values in the 32-bit field range.
const FIELD_WIDTH: i64 = 4_294_967_296;

/// A calendar's fields: the year, the month, the day and the time of day.
///
/// The month is counted from zero, so January is 0 and December is 11. The day
/// is counted from one. The weekday is counted from Sunday as zero and is an
/// answer, not an input: [`local_seconds`] and [`utc_seconds`] ignore it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fields {
    /// The civil year.
    pub year: i64,
    /// The month, from zero for January through eleven for December.
    pub month: i64,
    /// The day of the month, from one.
    pub day: i64,
    /// The hour of the day, from zero through twenty-three.
    pub hour: i64,
    /// The minute of the hour, from zero through fifty-nine.
    pub minute: i64,
    /// The second of the minute, from zero through fifty-nine.
    pub second: i64,
    /// The day of the week, from zero for Sunday through six for Saturday.
    pub weekday: i64,
}

/// The fields of an instant in UTC.
///
/// # Errors
///
/// [`DateError::InvalidTimestamp`] never occurs for an epoch that fits the
/// 64-bit seconds; the signature keeps the pair with [`utc_seconds`] uniform.
pub fn utc_fields(seconds: i64) -> Result<Fields, DateError> {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Ok(Fields {
        year,
        month,
        day,
        hour: of_day / SECONDS_PER_HOUR,
        minute: of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        second: of_day % SECONDS_PER_MINUTE,
        weekday: (days + EPOCH_WEEKDAY).rem_euclid(7),
    })
}

/// The fields of an instant at a zone `offset_seconds` east of UTC.
///
/// # Errors
///
/// [`DateError::InvalidTimestamp`] when the instant plus the offset does not
/// fit the epoch's 64 bits.
pub fn local_fields(seconds: i64, offset_seconds: i64) -> Result<Fields, DateError> {
    let shifted = seconds
        .checked_add(offset_seconds)
        .ok_or(DateError::InvalidTimestamp)?;
    utc_fields(shifted)
}

/// The instant the fields name in UTC.
///
/// # Errors
///
/// [`DateError::InvalidTimestamp`] when the fields do not fit the epoch's 64
/// bits, or when they name exactly -1 seconds — the value the field conversion
/// reserves for a set it cannot place.
pub fn utc_seconds(fields: &Fields) -> Result<i64, DateError> {
    let seconds = naive_seconds(fields)?;
    if seconds == -1 {
        return Err(DateError::InvalidTimestamp);
    }
    Ok(seconds)
}

/// The instant the fields name at a zone `offset_seconds` east of UTC.
///
/// # Errors
///
/// [`DateError::InvalidTimestamp`] when the fields do not fit the epoch's 64
/// bits, or when the local instant is exactly -1 seconds — the value the field
/// conversion reserves for a set it cannot place.
pub fn local_seconds(fields: &Fields, offset_seconds: i64) -> Result<i64, DateError> {
    let local = naive_seconds(fields)?
        .checked_sub(offset_seconds)
        .ok_or(DateError::InvalidTimestamp)?;
    if local == -1 {
        return Err(DateError::InvalidTimestamp);
    }
    Ok(local)
}

/// The instant the fields name in UTC, without the invalid-instant refusal.
///
/// The grammar builds a field set, places it at the system zone's standard
/// offset, checks that placement against the invalid-instant rule, and only
/// then applies the text's own declared zone. It therefore needs the
/// unrefused UTC instant as one step, and the callers of this function check
/// the placement themselves.
pub(crate) fn naive_seconds(fields: &Fields) -> Result<i64, DateError> {
    let year = i64::from(field32(fields.year).wrapping_sub(1900));
    let month = i64::from(field32(fields.month));
    let day = i64::from(field32(fields.day));
    let hour = i64::from(field32(fields.hour));
    let minute = i64::from(field32(fields.minute));
    let second = i64::from(field32(fields.second));
    let months = i128::from(year) * 12 + i128::from(month);
    let civil_year = months.div_euclid(12) + 1900;
    let civil_month = months.rem_euclid(12);
    let days = days_from_civil(civil_year, civil_month + 1)
        .checked_add(i128::from(day) - 1)
        .ok_or(DateError::InvalidTimestamp)?;
    let of_day = i128::from(hour) * i128::from(SECONDS_PER_HOUR)
        + i128::from(minute) * i128::from(SECONDS_PER_MINUTE)
        + i128::from(second);
    let seconds = days
        .checked_mul(i128::from(SECONDS_PER_DAY))
        .and_then(|midnight| midnight.checked_add(of_day))
        .ok_or(DateError::InvalidTimestamp)?;
    match i64::try_from(seconds) {
        Ok(seconds) => Ok(seconds),
        Err(_) => Err(DateError::InvalidTimestamp),
    }
}

/// The minutes a zone is west of UTC, as the class's offset member answers.
///
/// `offset_seconds` is the zone's offset east of UTC. A zone east of UTC is
/// west of it by a negative number of minutes, so `+09:00` answers `-540`.
///
/// # Errors
///
/// [`DateError::InvalidTimestamp`] when the offset is too negative to negate.
pub fn timezone_minutes(offset_seconds: i64) -> Result<i64, DateError> {
    let west = offset_seconds
        .checked_neg()
        .ok_or(DateError::InvalidTimestamp)?;
    Ok(west / SECONDS_PER_MINUTE)
}

/// The milliseconds an instant is, with the product wrapped at 64 bits.
pub fn milliseconds(seconds: i64) -> i64 {
    seconds.wrapping_mul(1_000)
}

/// The instant a millisecond count names, with the seconds truncated toward
/// zero.
pub fn seconds_from_milliseconds(milliseconds: i64) -> i64 {
    milliseconds / 1_000
}

/// The calendar date of a day count, as year, month from zero and day from
/// one.
///
/// The algorithm counts the days of the proleptic Gregorian calendar from
/// 1970-01-01, so a day before the epoch is a negative count and the calendar
/// carries into the year before it.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted - era * 146_097;
    let of_year = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let year = of_year + era * 400;
    let of_year_days = of_era - (365 * of_year + of_year / 4 - of_year / 100);
    let of_march = (5 * of_year_days + 2) / 153;
    let day = of_year_days - (153 * of_march + 2) / 5 + 1;
    let month = of_march + if of_march < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month - 1, day)
}

/// The day count of the first day of a civil month, counted from 1970-01-01.
///
/// `month` is counted from one. The calendar is proleptic Gregorian, so the
/// count is defined for every year, including the years before the epoch.
fn days_from_civil(year: i128, month: i128) -> i128 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_march = month + if month > 2 { -3 } else { 9 };
    let of_year = (153 * of_march + 2) / 5;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * DAYS_PER_ERA + of_era_days - 719_468
}

/// A field as the 32-bit signed integer the calendar reads.
///
/// A value inside the range is itself. A value outside it keeps its low 32
/// bits, two's complement: the arithmetic is the same as a narrowing
/// conversion, written so that no value wraps by accident.
fn field32(value: i64) -> i32 {
    let low = value.rem_euclid(FIELD_WIDTH);
    let bits = u32::try_from(low).expect("a remainder below 2^32 fits in a u32");
    i32::from_ne_bytes(bits.to_ne_bytes())
}

/// The failure of a calendar conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DateError {
    /// The text is not a date the grammar reads.
    #[error("Invalid date format")]
    CannotParse,
    /// The fields name no instant, or the instant does not fit the epoch.
    #[error("Invalid value for date/time")]
    InvalidTimestamp,
}

#[cfg(test)]
mod tests {
    use super::{
        DateError, Fields, local_fields, local_seconds, milliseconds, utc_fields, utc_seconds,
    };
    use super::{seconds_from_milliseconds, timezone_minutes};

    /// A field set with the time of day and an unused weekday.
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

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:56-88
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:300-308
    #[test]
    fn the_epoch_is_1970_and_the_weekday_counts_from_sunday() {
        let epoch = utc_fields(0).expect("the epoch has fields");
        assert_eq!(epoch.year, 1970);
        assert_eq!(epoch.month, 0);
        assert_eq!(epoch.day, 1);
        assert_eq!(epoch.hour, 0);
        assert_eq!(epoch.minute, 0);
        assert_eq!(epoch.second, 0);
        assert_eq!(epoch.weekday, 4);
        let day_before = utc_fields(-1).expect("the second before has fields");
        assert_eq!(
            (
                day_before.year,
                day_before.month,
                day_before.day,
                day_before.hour
            ),
            (1969, 11, 31, 23)
        );
        assert_eq!(day_before.weekday, 3);
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:63-88
    #[test]
    fn a_field_outside_its_range_carries_into_the_next_unit() {
        let month = local_seconds(&fields(2020, 12, 1, 0, 0, 0), 0)
            .and_then(|seconds| local_fields(seconds, 0))
            .expect("the month carries");
        assert_eq!((month.year, month.month, month.day), (2021, 0, 1));
        let month_before = local_seconds(&fields(2020, -1, 1, 0, 0, 0), 0)
            .and_then(|seconds| local_fields(seconds, 0))
            .expect("the month carries back");
        assert_eq!(
            (month_before.year, month_before.month, month_before.day),
            (2019, 11, 1)
        );
        let day = local_seconds(&fields(2020, 0, 32, 0, 0, 0), 0)
            .and_then(|seconds| local_fields(seconds, 0))
            .expect("the day carries");
        assert_eq!((day.year, day.month, day.day), (2020, 1, 1));
        let day_before = local_seconds(&fields(2020, 0, 0, 0, 0, 0), 0)
            .and_then(|seconds| local_fields(seconds, 0))
            .expect("the day carries back");
        assert_eq!(
            (day_before.year, day_before.month, day_before.day),
            (2019, 11, 31)
        );
        let hour = local_seconds(&fields(2020, 0, 1, 25, 0, 0), 0)
            .and_then(|seconds| local_fields(seconds, 0))
            .expect("the hour carries");
        assert_eq!(
            (hour.year, hour.month, hour.day, hour.hour, hour.minute),
            (2020, 0, 2, 1, 0)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:64-78
    #[test]
    fn a_field_is_read_as_a_32_bit_integer() {
        let wrapped = utc_fields(
            local_seconds(&fields(4_294_969_296, 0, 1, 0, 0, 0), 0).expect("the year wraps"),
        )
        .expect("the wrapped year has fields");
        assert_eq!(wrapped.year, 2000);
        let low = utc_fields(
            local_seconds(&fields(-4_294_965_296, 0, 1, 0, 0, 0), 0).expect("the year wraps"),
        )
        .expect("the wrapped year has fields");
        assert_eq!(low.year, 2000);
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:74-127
    #[test]
    fn the_zone_moves_the_instant_and_the_fields_together() {
        let local = local_seconds(&fields(2004, 4, 3, 11, 22, 33), 32_400)
            .expect("the local fields name an instant");
        assert_eq!(local, 1_083_550_953);
        let read = local_fields(local, 32_400).expect("the instant has local fields");
        assert_eq!(
            (
                read.year,
                read.month,
                read.day,
                read.hour,
                read.minute,
                read.second
            ),
            (2004, 4, 3, 11, 22, 33)
        );
        let utc = utc_fields(local).expect("the instant has UTC fields");
        assert_eq!((utc.hour, utc.minute, utc.second), (2, 22, 33));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:84-86
    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:116-118
    #[test]
    fn an_invalid_instant_is_refused() {
        assert_eq!(
            local_seconds(&fields(1970, 0, 1, 8, 59, 59), 32_400),
            Err(DateError::InvalidTimestamp)
        );
        assert_eq!(
            utc_seconds(&fields(1969, 11, 31, 23, 59, 59)),
            Err(DateError::InvalidTimestamp)
        );
        assert!(local_seconds(&fields(1970, 0, 1, 8, 59, 58), 32_400).is_ok());
        assert_eq!(utc_seconds(&fields(1970, 0, 1, 0, 0, 0)), Ok(0));
        assert_eq!(utc_seconds(&fields(1969, 11, 31, 23, 59, 58)), Ok(-2));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:300-315
    #[test]
    fn the_milliseconds_truncate_and_the_zone_answers_west() {
        assert_eq!(milliseconds(1_083_550_953), 1_083_550_953_000);
        assert_eq!(seconds_from_milliseconds(1_500), 1);
        assert_eq!(seconds_from_milliseconds(-1_500), -1);
        assert_eq!(timezone_minutes(32_400), Ok(-540));
        assert_eq!(timezone_minutes(-18_000), Ok(300));
        assert_eq!(timezone_minutes(0), Ok(0));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:63-88
    #[test]
    fn a_leap_day_is_a_calendar_day_and_a_common_year_has_none() {
        let leap =
            utc_fields(utc_seconds(&fields(2020, 1, 29, 12, 0, 0)).expect("the day is valid"))
                .expect("the instant has fields");
        assert_eq!((leap.month, leap.day), (1, 29));
        let common =
            utc_fields(utc_seconds(&fields(2019, 1, 29, 12, 0, 0)).expect("the day carries"))
                .expect("the instant has fields");
        assert_eq!((common.year, common.month, common.day), (2019, 2, 1));
    }
}
