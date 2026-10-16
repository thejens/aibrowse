//! Today's date in words, for Jev: goals are often relative ("tomorrow's trains", "this
//! weekend's forecast", "available tables tomorrow"), and a page shows absolute dates
//! ("14 Oct 2026"). Without today's date, OpenTable's date picker, labelled with today's
//! date, read as just a date, and tomorrow's times were never asked for.

use std::time::{SystemTime, UNIX_EPOCH};

const WEEKDAYS: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Today's date (UTC) as "Wednesday 14 October 2026".
pub(crate) fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    in_words(days)
}

/// A day counted from 1 January 1970, in words: the civil calendar from days
/// (Howard Hinnant's algorithm).
fn in_words(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let weekday = WEEKDAYS[days.rem_euclid(7) as usize];
    format!("{weekday} {day} {} {year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_read_as_dates() {
        assert_eq!(in_words(0), "Thursday 1 January 1970");
        assert_eq!(in_words(20_740), "Wednesday 14 October 2026");
        assert_eq!(in_words(11_016), "Tuesday 29 February 2000");
    }
}
