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
//!
//! # The class table
//!
//! [`DateMember`] is the class's surface: the accessors, the setters, the
//! millisecond pair, the zone offset and the text grammar, each with the rule
//! that answers it. [`Date`] is the state a call reads and writes: one instant
//! and the zone its fields are placed in. [`Date::new`] is the construction's
//! own rule: no argument is the clock, one text is the grammar, and the rest
//! are the fields, where a void argument is absent.

use std::time::{SystemTime, UNIX_EPOCH};

use super::date_parse;
use crate::vm::runtime::natives::string::Arity;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Target, Variant, VariantError};

/// The name the class is registered under.
pub const CLASS_NAME: &str = "Date";

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

/// A member of the date class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateMember {
    /// The empty finalizer every instance carries.
    Finalize,
    /// Replaces the year.
    SetYear,
    /// Replaces the month, counted from zero.
    SetMonth,
    /// Replaces the day of the month.
    SetDate,
    /// Replaces the hour of the day.
    SetHours,
    /// Replaces the minute of the hour.
    SetMinutes,
    /// Replaces the second of the minute.
    SetSeconds,
    /// Replaces the instant from a millisecond count.
    SetTime,
    /// The day of the month.
    GetDate,
    /// The day of the week, from Sunday as zero.
    GetDay,
    /// The hour of the day.
    GetHours,
    /// The minute of the hour.
    GetMinutes,
    /// The month, counted from zero.
    GetMonth,
    /// The second of the minute.
    GetSeconds,
    /// The instant, in milliseconds.
    GetTime,
    /// The zone's offset in minutes west of UTC.
    GetTimezoneOffset,
    /// The civil year.
    GetYear,
    /// Replaces the instant from a text.
    Parse,
}

impl DateMember {
    /// Every member, in the order the class registers them.
    pub const ALL: &'static [DateMember] = &[
        Self::Finalize,
        Self::SetYear,
        Self::SetMonth,
        Self::SetDate,
        Self::SetHours,
        Self::SetMinutes,
        Self::SetSeconds,
        Self::SetTime,
        Self::GetDate,
        Self::GetDay,
        Self::GetHours,
        Self::GetMinutes,
        Self::GetMonth,
        Self::GetSeconds,
        Self::GetTime,
        Self::GetTimezoneOffset,
        Self::GetYear,
        Self::Parse,
    ];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        Self::ALL
            .iter()
            .copied()
            .find(|member| units.iter().copied().eq(member.name().encode_utf16()))
    }

    /// The name the class publishes the member under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Finalize => "finalize",
            Self::SetYear => "setYear",
            Self::SetMonth => "setMonth",
            Self::SetDate => "setDate",
            Self::SetHours => "setHours",
            Self::SetMinutes => "setMinutes",
            Self::SetSeconds => "setSeconds",
            Self::SetTime => "setTime",
            Self::GetDate => "getDate",
            Self::GetDay => "getDay",
            Self::GetHours => "getHours",
            Self::GetMinutes => "getMinutes",
            Self::GetMonth => "getMonth",
            Self::GetSeconds => "getSeconds",
            Self::GetTime => "getTime",
            Self::GetTimezoneOffset => "getTimezoneOffset",
            Self::GetYear => "getYear",
            Self::Parse => "parse",
        }
    }

    /// The parameter counts a call accepts.
    ///
    /// An accessor ignores the parameters after the ones it reads, and a
    /// setter and the grammar need their one parameter.
    pub fn arity(self) -> Arity {
        match self {
            Self::SetYear
            | Self::SetMonth
            | Self::SetDate
            | Self::SetHours
            | Self::SetMinutes
            | Self::SetSeconds
            | Self::SetTime
            | Self::Parse => Arity::AtLeast(1),
            Self::Finalize
            | Self::GetDate
            | Self::GetDay
            | Self::GetHours
            | Self::GetMinutes
            | Self::GetMonth
            | Self::GetSeconds
            | Self::GetTime
            | Self::GetTimezoneOffset
            | Self::GetYear => Arity::AtLeast(0),
        }
    }

    /// Calls the member on `date`.
    ///
    /// A setter reads the local fields, replaces the one field it owns and
    /// places the result back at the same offset. The grammar's refusal and
    /// the calendar's invalid instant are the date's own failures; a parameter
    /// that has no integer or no text is a conversion failure.
    ///
    /// # Errors
    ///
    /// [`DateMemberError::ParameterCount`] when the call gives a count the
    /// member does not accept, [`DateMemberError::Convert`] when a parameter
    /// has no kind the rule reads, and [`DateError`] when the fields or the
    /// text name no instant.
    pub fn call(self, date: &mut Date, parameters: &[Variant]) -> Result<Variant, DateMemberError> {
        let arity = self.arity();
        if !arity.accepts(parameters.len()) {
            return Err(DateMemberError::ParameterCount { arity });
        }
        match self {
            Self::Finalize => Ok(Variant::Void),
            Self::SetYear => set_field(date, parameters, |fields| &mut fields.year),
            Self::SetMonth => set_field(date, parameters, |fields| &mut fields.month),
            Self::SetDate => set_field(date, parameters, |fields| &mut fields.day),
            Self::SetHours => set_field(date, parameters, |fields| &mut fields.hour),
            Self::SetMinutes => set_field(date, parameters, |fields| &mut fields.minute),
            Self::SetSeconds => set_field(date, parameters, |fields| &mut fields.second),
            Self::SetTime => {
                date.seconds = seconds_from_milliseconds(parameters[0].to_integer()?);
                Ok(Variant::Void)
            }
            Self::GetDate => get_field(date, |fields| fields.day),
            Self::GetDay => get_field(date, |fields| fields.weekday),
            Self::GetHours => get_field(date, |fields| fields.hour),
            Self::GetMinutes => get_field(date, |fields| fields.minute),
            Self::GetMonth => get_field(date, |fields| fields.month),
            Self::GetSeconds => get_field(date, |fields| fields.second),
            Self::GetTime => Ok(Variant::Integer(milliseconds(date.seconds))),
            Self::GetTimezoneOffset => Ok(Variant::Integer(timezone_minutes(date.offset)?)),
            Self::GetYear => get_field(date, |fields| fields.year),
            Self::Parse => {
                let Variant::String(text) = &parameters[0] else {
                    return Err(VariantError::Convert {
                        to: Target::Text.name(),
                    }
                    .into());
                };
                date.seconds = date_parse::parse(text.as_utf16(), date.offset)?;
                Ok(Variant::Void)
            }
        }
    }
}

/// A date: one instant and the zone its fields are read and written at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Date {
    /// The instant, in seconds from 1970-01-01 00:00:00 UTC.
    pub seconds: i64,
    /// The zone's offset, in seconds east of UTC.
    pub offset: i64,
}

impl Date {
    /// The date a construction's arguments name, at `offset` seconds east of
    /// UTC.
    ///
    /// No argument is the clock. One text is read by the grammar at the zone
    /// the text names, or at `offset` when it names none. Any other first
    /// argument is the year, and the arguments after it are the month, the
    /// day, the hour, the minute and the second; a field that is absent or
    /// void keeps its default, and the arguments after the sixth are ignored.
    ///
    /// # Errors
    ///
    /// [`DateMemberError::Convert`] when a field has no integer, and
    /// [`DateError`] when the text or the field set names no instant.
    pub fn new(arguments: &[Variant], offset: i64) -> Result<Self, DateMemberError> {
        let seconds = match arguments.first() {
            None => now_seconds()?,
            Some(Variant::String(text)) => date_parse::parse(text.as_utf16(), offset)?,
            Some(year) => {
                let mut fields = Fields {
                    year: year.to_integer()?,
                    month: 0,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    weekday: 0,
                };
                let slots: [&mut i64; 5] = [
                    &mut fields.month,
                    &mut fields.day,
                    &mut fields.hour,
                    &mut fields.minute,
                    &mut fields.second,
                ];
                for (position, slot) in slots.into_iter().enumerate() {
                    let Some(value) = arguments.get(position + 1) else {
                        break;
                    };
                    if !matches!(value, Variant::Void) {
                        *slot = value.to_integer()?;
                    }
                }
                local_seconds(&fields, offset)?
            }
        };
        Ok(Self { seconds, offset })
    }
}

/// The current instant, in seconds from 1970-01-01 00:00:00 UTC.
fn now_seconds() -> Result<i64, DateError> {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).map_err(|_| DateError::InvalidTimestamp),
        Err(error) => {
            let seconds = i64::try_from(error.duration().as_secs())
                .map_err(|_| DateError::InvalidTimestamp)?;
            seconds.checked_neg().ok_or(DateError::InvalidTimestamp)
        }
    }
}

/// The local fields with the one the call names taken from its parameter.
fn set_field(
    date: &mut Date,
    parameters: &[Variant],
    select: for<'a> fn(&'a mut Fields) -> &'a mut i64,
) -> Result<Variant, DateMemberError> {
    let value = parameters[0].to_integer()?;
    let mut fields = local_fields(date.seconds, date.offset)?;
    *select(&mut fields) = value;
    date.seconds = local_seconds(&fields, date.offset)?;
    Ok(Variant::Void)
}

/// One local field, as the accessor answers it.
fn get_field(date: &Date, select: fn(&Fields) -> i64) -> Result<Variant, DateMemberError> {
    let fields = local_fields(date.seconds, date.offset)?;
    Ok(Variant::Integer(select(&fields)))
}

/// The failure of a date member's call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DateMemberError {
    /// The call gives a number of parameters the member does not accept.
    #[error("the member does not accept this many parameters")]
    ParameterCount {
        /// The counts the member accepts.
        arity: Arity,
    },
    /// The calendar refused the field set or the text.
    #[error(transparent)]
    Date(#[from] DateError),
    /// A parameter has no conversion the rule needs.
    #[error(transparent)]
    Convert(#[from] VariantError),
}

#[cfg(test)]
mod tests {
    use super::{
        Date, DateError, DateMember, DateMemberError, Fields, local_fields, local_seconds,
        milliseconds, utc_fields, utc_seconds,
    };
    use super::{seconds_from_milliseconds, timezone_minutes};
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::Variant;

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

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:46-345
    #[test]
    fn the_member_table_is_the_class_surface() {
        let names: Vec<&str> = DateMember::ALL.iter().map(|member| member.name()).collect();
        assert_eq!(
            names.as_slice(),
            &[
                "finalize",
                "setYear",
                "setMonth",
                "setDate",
                "setHours",
                "setMinutes",
                "setSeconds",
                "setTime",
                "getDate",
                "getDay",
                "getHours",
                "getMinutes",
                "getMonth",
                "getSeconds",
                "getTime",
                "getTimezoneOffset",
                "getYear",
                "parse",
            ]
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:53-93
    #[test]
    fn a_construction_reads_its_fields_at_the_zone() {
        let date = Date::new(
            &[
                Variant::Integer(2020),
                Variant::Integer(5),
                Variant::Integer(15),
                Variant::Integer(12),
                Variant::Integer(30),
                Variant::Integer(45),
            ],
            32_400,
        )
        .expect("the fields name an instant");
        let fields = local_fields(date.seconds, date.offset).expect("the instant has fields");
        assert_eq!(
            (
                fields.year,
                fields.month,
                fields.day,
                fields.hour,
                fields.minute,
                fields.second
            ),
            (2020, 5, 15, 12, 30, 45)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:53-93
    #[test]
    fn a_void_field_is_absent_and_an_extra_field_is_ignored() {
        let one = Date::new(&[Variant::Integer(2020)], 32_400).expect("the year is a date");
        let void = Date::new(
            &[Variant::Integer(2020), Variant::Void, Variant::Void],
            32_400,
        )
        .expect("the year is a date");
        assert_eq!(one.seconds, void.seconds);
        let fields = local_fields(void.seconds, void.offset).expect("the instant has fields");
        assert_eq!((fields.year, fields.month, fields.day), (2020, 0, 1));
        let six = Date::new(
            &[
                Variant::Integer(2020),
                Variant::Integer(5),
                Variant::Integer(15),
                Variant::Integer(12),
                Variant::Integer(30),
                Variant::Integer(45),
            ],
            32_400,
        )
        .expect("the fields name an instant");
        let seven = Date::new(
            &[
                Variant::Integer(2020),
                Variant::Integer(5),
                Variant::Integer(15),
                Variant::Integer(12),
                Variant::Integer(30),
                Variant::Integer(45),
                Variant::Integer(999),
            ],
            32_400,
        )
        .expect("the fields name an instant");
        assert_eq!(six.seconds, seven.seconds);
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:28-32
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:62
    #[test]
    fn a_text_construction_reads_the_grammar() {
        let zoned = Date::new(
            &[Variant::String(TjsString::from("1 Jan 1970 09:00:00 JST"))],
            32_400,
        )
        .expect("the text names an instant");
        assert_eq!(zoned.seconds, 0);
        let local = Date::new(
            &[Variant::String(TjsString::from("1 Jan 1970 09:00:00"))],
            32_400,
        )
        .expect("the text names an instant");
        assert_eq!(local.seconds, 0);
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:95-220
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:222-332
    #[test]
    fn a_setter_places_its_field_at_the_zone() {
        let mut date = Date::new(
            &[
                Variant::Integer(2004),
                Variant::Integer(4),
                Variant::Integer(3),
                Variant::Integer(11),
                Variant::Integer(22),
                Variant::Integer(33),
            ],
            32_400,
        )
        .expect("the fields name an instant");
        let answer = DateMember::call(DateMember::SetYear, &mut date, &[Variant::Integer(1999)])
            .expect("the year is a field");
        assert!(matches!(answer, Variant::Void));
        let year = DateMember::call(DateMember::GetYear, &mut date, &[])
            .expect("the year reads back")
            .to_integer()
            .expect("the year is an integer");
        assert_eq!(year, 1999);
        DateMember::call(DateMember::SetTime, &mut date, &[Variant::Integer(1_500)])
            .expect("the millisecond count is an instant");
        let time = DateMember::call(DateMember::GetTime, &mut date, &[])
            .expect("the instant reads back")
            .to_integer()
            .expect("the instant is an integer");
        assert_eq!(time, 1_000);
        let zone = DateMember::call(DateMember::GetTimezoneOffset, &mut date, &[])
            .expect("the zone answers")
            .to_integer()
            .expect("the zone is an integer");
        assert_eq!(zone, -540);
    }

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:99-100
    #[test]
    fn a_member_that_needs_its_parameter_refuses_a_call_without_one() {
        let mut date = Date::new(&[Variant::Integer(2020)], 32_400).expect("the year is a date");
        let answer = DateMember::call(DateMember::SetYear, &mut date, &[]);
        assert!(matches!(
            answer,
            Err(DateMemberError::ParameterCount { .. })
        ));
    }
}
