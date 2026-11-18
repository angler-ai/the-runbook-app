//! LANG9b: schedule triggers' cron expressions. The standard five fields
//! (minute, hour, day of month, month, day of week) with `*`, lists, ranges
//! and steps, month and weekday names, and Sunday as 0 or 7. As in cron,
//! when both the day of month and the day of week are restricted, either
//! matching is enough. Times are matched in a named IANA time zone.
use chrono::{Datelike, TimeZone, Timelike};

/// The longest window a pass looks back for a due tick (24 hours).
pub const MAX_LOOKBACK_MS: i64 = 24 * 60 * 60 * 1000;
const MINUTE_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    day_star: bool,
    weekday_star: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn value(text: &str, min: u32, max: u32, names: &[&str]) -> Option<u32> {
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(text)) {
        return Some(min + i as u32);
    }
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = text.parse().ok()?;
    (min..=max).contains(&n).then_some(n)
}

/// One field as a bit set, and whether it was `*` (or `*/1`).
fn field(text: &str, min: u32, max: u32, names: &[&str]) -> Option<(u64, bool)> {
    let mut bits = 0u64;
    let mut star = false;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, value(step, 1, max.max(1), &[])?),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            if part.split(',').count() == 1 && step == 1 && text == part {
                star = true;
            }
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (value(a, min, max, names)?, value(b, min, max, names)?);
            if a > b {
                return None;
            }
            (a, b)
        } else {
            let a = value(range, min, max, names)?;
            // `5/15` means from 5 to the end, every 15.
            (a, if part.contains('/') { max } else { a })
        };
        let mut n = lo;
        while n <= hi {
            bits |= 1 << n;
            n += step;
        }
    }
    (bits != 0).then_some((bits, star))
}

impl Cron {
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = parts.as_slice() else {
            return None;
        };
        let (minutes, _) = field(minute, 0, 59, &[])?;
        let (hours, _) = field(hour, 0, 23, &[])?;
        let (days, day_star) = field(day, 1, 31, &[])?;
        let (months, _) = field(month, 1, 12, &MONTHS)?;
        let (mut weekdays, weekday_star) = field(weekday, 0, 7, &WEEKDAYS)?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays | 1) & !(1 << 7);
        }
        Some(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            day_star,
            weekday_star,
        })
    }

    fn matches<T: TimeZone>(&self, at: &chrono::DateTime<T>) -> bool {
        let has = |bits: u64, n: u32| bits & (1 << n) != 0;
        let day = has(self.days, at.day());
        let weekday = has(self.weekdays, at.weekday().num_days_from_sunday());
        let date = match (self.day_star, self.weekday_star) {
            (true, true) => true,
            (true, false) => weekday,
            (false, true) => day,
            (false, false) => day || weekday,
        };
        date && has(self.minutes, at.minute())
            && has(self.hours, at.hour())
            && has(self.months, at.month())
    }

    /// The latest tick at or before `now_ms` and after `now_ms - window_ms`
    /// (whole minutes, UTC milliseconds), in time zone `tz`. A local time
    /// skipped by a daylight-saving change never ticks; one repeated ticks
    /// once, at its first occurrence.
    pub fn last_tick(&self, tz: chrono_tz::Tz, now_ms: i64, window_ms: i64) -> Option<i64> {
        let window = window_ms.clamp(0, MAX_LOOKBACK_MS);
        let mut minute = now_ms.div_euclid(MINUTE_MS) * MINUTE_MS;
        let stop = now_ms - window;
        while minute > stop {
            let at = tz.timestamp_millis_opt(minute).single()?;
            if self.matches(&at) {
                // A repeated local time ticks only at its first occurrence:
                // skip this one when the same local time an hour earlier
                // also matched (the fall-back hour).
                let earlier = tz.timestamp_millis_opt(minute - 60 * MINUTE_MS).single()?;
                if earlier.naive_local() == at.naive_local() {
                    minute -= MINUTE_MS;
                    continue;
                }
                return Some(minute);
            }
            minute -= MINUTE_MS;
        }
        None
    }
}

/// A named IANA time zone (`UTC`, `America/Los_Angeles`).
pub fn time_zone(name: &str) -> Option<chrono_tz::Tz> {
    name.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, tz: chrono_tz::Tz) -> i64 {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .earliest()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn parses_the_standard_forms() {
        for ok in [
            "* * * * *",
            "0 2 * * *",
            "*/15 9-17 * * mon-fri",
            "0 0 1,15 * *",
            "30 6 * jan,jul sun",
            "0 0 * * 7",
            "5/10 * * * *",
        ] {
            assert!(Cron::parse(ok).is_some(), "{ok}");
        }
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "5-1 * * * *",
            "*/0 * * * *",
            "a * * * *",
        ] {
            assert!(Cron::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn finds_the_latest_tick_inside_the_window() {
        let utc = chrono_tz::UTC;
        let cron = Cron::parse("0 2 * * *").unwrap();
        let tick = ms(2026, 11, 15, 2, 0, utc);
        assert_eq!(cron.last_tick(utc, tick + 30_000, 600_000), Some(tick));
        assert_eq!(cron.last_tick(utc, tick + 9 * 60_000, 600_000), Some(tick));
        assert_eq!(cron.last_tick(utc, tick + 11 * 60_000, 600_000), None);
        assert_eq!(cron.last_tick(utc, tick - 60_000, 600_000), None);
    }

    #[test]
    fn matches_in_the_named_time_zone() {
        let la: chrono_tz::Tz = time_zone("America/Los_Angeles").unwrap();
        let cron = Cron::parse("0 9 * * mon-fri").unwrap();
        // Monday 2026-11-16 09:00 in Los Angeles.
        let tick = ms(2026, 11, 16, 9, 0, la);
        assert_eq!(cron.last_tick(la, tick, 60_000), Some(tick));
        // Sunday: no tick.
        let sunday = ms(2026, 11, 15, 9, 0, la);
        assert_eq!(cron.last_tick(la, sunday, 60_000), None);
        assert!(time_zone("Mars/Base").is_none());
    }

    #[test]
    fn day_of_month_or_weekday_when_both_are_set() {
        let utc = chrono_tz::UTC;
        let cron = Cron::parse("0 0 1 * mon").unwrap();
        // 2026-11-02 is a Monday, not the 1st: still a tick.
        assert!(
            cron.last_tick(utc, ms(2026, 11, 2, 0, 0, utc), 60_000)
                .is_some()
        );
        assert!(
            cron.last_tick(utc, ms(2026, 12, 1, 0, 0, utc), 60_000)
                .is_some()
        );
        assert!(
            cron.last_tick(utc, ms(2026, 11, 3, 0, 0, utc), 60_000)
                .is_none()
        );
    }

    #[test]
    fn a_repeated_local_hour_ticks_once() {
        let la: chrono_tz::Tz = time_zone("America/Los_Angeles").unwrap();
        let cron = Cron::parse("30 1 * * *").unwrap();
        // 2026-11-01: 01:30 happens twice in Los Angeles.
        let first = la
            .with_ymd_and_hms(2026, 11, 1, 1, 30, 0)
            .earliest()
            .unwrap()
            .timestamp_millis();
        let second = first + 3_600_000;
        assert_eq!(cron.last_tick(la, first, 60_000), Some(first));
        assert_eq!(cron.last_tick(la, second, 60_000), None);
    }
}
