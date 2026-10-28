//! Date helpers: moment-style formatting, day arithmetic, and local time.
//!
//! No external date crate: we convert to/from "days since 1970-01-01" using
//! Howard Hinnant's algorithms and format with a small subset of moment.js
//! tokens (what Obsidian's daily-note date format uses).

use std::time::{SystemTime, UNIX_EPOCH};

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
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Full month name (1 = January).
pub fn month_name(m: u32) -> &'static str {
    MONTHS[(m.clamp(1, 12) as usize) - 1]
}

/// Short weekday labels, Sunday-first.
pub const WEEKDAY_SHORT: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date `(year, month, day)` for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Weekday index with 0 = Sunday.
pub fn weekday(y: i32, m: u32, d: u32) -> usize {
    let days = days_from_civil(y, m, d);
    (((days % 7) + 7 + 4) % 7) as usize
}

/// Today as `(year, month, day)`.
pub fn today() -> (i32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days((secs / 86_400) as i64)
}

/// Shift a civil date by `delta` days.
pub fn add_days(y: i32, m: u32, d: u32, delta: i64) -> (i32, u32, u32) {
    civil_from_days(days_from_civil(y, m, d) + delta)
}

/// Format a civil date using moment.js-style tokens.
///
/// Supported: `YYYY YY MMMM MMM MM M DD D dddd ddd`.
pub fn format(fmt: &str, y: i32, m: u32, d: u32) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if rest.starts_with("YYYY") {
            out.push_str(&format!("{y:04}"));
            i += 4;
        } else if rest.starts_with("YY") {
            out.push_str(&format!("{:02}", y.rem_euclid(100)));
            i += 2;
        } else if rest.starts_with("MMMM") {
            out.push_str(MONTHS[(m as usize - 1).min(11)]);
            i += 4;
        } else if rest.starts_with("MMM") {
            out.push_str(&MONTHS[(m as usize - 1).min(11)][..3]);
            i += 3;
        } else if rest.starts_with("MM") {
            out.push_str(&format!("{m:02}"));
            i += 2;
        } else if rest.starts_with("DD") {
            out.push_str(&format!("{d:02}"));
            i += 2;
        } else if rest.starts_with("dddd") {
            out.push_str(WEEKDAYS[weekday(y, m, d)]);
            i += 4;
        } else if rest.starts_with("ddd") {
            out.push_str(&WEEKDAYS[weekday(y, m, d)][..3]);
            i += 3;
        } else if rest.starts_with('M') {
            out.push_str(&m.to_string());
            i += 1;
        } else if rest.starts_with('D') {
            out.push_str(&d.to_string());
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Local time as `(hour, minute, second)`, falling back to UTC.
pub fn now_hms() -> (u32, u32, u32) {
    if let Ok(out) = std::process::Command::new("date").arg("+%H %M %S").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let nums: Vec<u32> = s
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if nums.len() == 3 {
                return (nums[0], nums[1], nums[2]);
            }
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (
        ((secs / 3600) % 24) as u32,
        ((secs / 60) % 60) as u32,
        (secs % 60) as u32,
    )
}

/// Format a time with tokens `HH H mm ss A a`.
pub fn format_time(fmt: &str, h: u32, m: u32, s: u32) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if rest.starts_with("HH") {
            out.push_str(&format!("{h:02}"));
            i += 2;
        } else if rest.starts_with("mm") {
            out.push_str(&format!("{m:02}"));
            i += 2;
        } else if rest.starts_with("ss") {
            out.push_str(&format!("{s:02}"));
            i += 2;
        } else if rest.starts_with("H") {
            out.push_str(&h.to_string());
            i += 1;
        } else if rest.starts_with('A') {
            out.push_str(if h < 12 { "AM" } else { "PM" });
            i += 1;
        } else if rest.starts_with('a') {
            out.push_str(if h < 12 { "am" } else { "pm" });
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Number of days in a month.
pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Shift a year/month by `delta` months.
pub fn add_months(y: i32, m: u32, delta: i32) -> (i32, u32) {
    let total = y * 12 + (m as i32 - 1) + delta;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) as u32 + 1;
    (ny, nm)
}

/// A month laid out as weeks of 7 days (Sunday-first), `None` for padding.
pub fn month_grid(y: i32, m: u32) -> Vec<Vec<Option<u32>>> {
    let first = weekday(y, m, 1);
    let dim = days_in_month(y, m);
    let mut cells: Vec<Option<u32>> = vec![None; first];
    for d in 1..=dim {
        cells.push(Some(d));
    }
    while !cells.len().is_multiple_of(7) {
        cells.push(None);
    }
    cells.chunks(7).map(|c| c.to_vec()).collect()
}

/// Expand Obsidian-style placeholders in a template.
///
/// Supports `{{title}}`, `{{date}}`, `{{date:FMT}}`, `{{time}}`, `{{time:FMT}}`
/// where `FMT` uses moment-style tokens. `ctx` is the note's date.
pub fn expand(template: &str, title: &str, ctx: (i32, u32, u32), default_date_fmt: &str) -> String {
    let (cy, cm, cd) = ctx;
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        let (name, fmt) = match key.split_once(':') {
            Some((n, f)) => (n.trim(), Some(f)),
            None => (key.trim(), None),
        };
        let value = match name {
            "title" => title.to_string(),
            "date" => format(fmt.unwrap_or(default_date_fmt), cy, cm, cd),
            "time" => {
                let (h, m, s) = now_hms();
                match fmt {
                    Some(f) => format_time(f, h, m, s),
                    None => format!("{h:02}:{m:02}"),
                }
            }
            _ => format!("{{{{{key}}}}}"),
        };
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        for &(y, m, d) in &[(2024, 1, 1), (1999, 12, 31), (2000, 2, 29)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn formats_default_token() {
        assert_eq!(format("YYYY-MM-DD", 2024, 1, 5), "2024-01-05");
    }

    #[test]
    fn formats_verbose_tokens() {
        // 2024-01-01 is a Monday.
        assert_eq!(
            format("dddd, MMMM D, YYYY", 2024, 1, 1),
            "Monday, January 1, 2024"
        );
        assert_eq!(format("ddd DD/MM/YY", 2024, 3, 9), "Sat 09/03/24");
    }

    #[test]
    fn weekday_is_correct() {
        assert_eq!(WEEKDAYS[weekday(1970, 1, 1)], "Thursday");
        assert_eq!(WEEKDAYS[weekday(2024, 1, 1)], "Monday");
    }

    #[test]
    fn adds_days_across_month_and_year() {
        assert_eq!(add_days(2024, 1, 31, 1), (2024, 2, 1));
        assert_eq!(add_days(2024, 12, 31, 1), (2025, 1, 1));
        assert_eq!(add_days(2024, 1, 1, -1), (2023, 12, 31));
    }

    #[test]
    fn month_lengths_and_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2100, 2), 28); // 2100 is not a leap year
        assert_eq!(days_in_month(2000, 2), 29); // but 2000 is
        assert_eq!(days_in_month(2024, 4), 30);
    }

    #[test]
    fn calendar_grid_2024_01() {
        // 2024-01-01 is a Monday, so the first row is [None, 1..6].
        let grid = month_grid(2024, 1);
        assert_eq!(grid[0][0], None);
        assert_eq!(grid[0][1], Some(1));
        assert_eq!(grid[0][6], Some(6));
        assert_eq!(
            *grid.last().unwrap(),
            vec![Some(28), Some(29), Some(30), Some(31), None, None, None]
        );
    }

    #[test]
    fn month_navigation_wraps_years() {
        assert_eq!(add_months(2024, 12, 1), (2025, 1));
        assert_eq!(add_months(2024, 1, -1), (2023, 12));
        assert_eq!(add_months(2024, 6, 0), (2024, 6));
    }

    #[test]
    fn format_time_tokens() {
        assert_eq!(format_time("HH:mm", 9, 5, 0), "09:05");
        assert_eq!(format_time("H:mm A", 15, 30, 0), "15:30 PM");
    }

    #[test]
    fn expands_placeholders() {
        let ctx = (2024, 3, 9);
        assert_eq!(
            expand("# {{title}} — {{date:dddd}}", "Hello", ctx, "YYYY-MM-DD"),
            "# Hello — Saturday"
        );
        assert_eq!(expand("{{date}}", "x", ctx, "YYYY-MM-DD"), "2024-03-09");
        assert_eq!(
            expand("{{date:DD.MM.YY}}", "x", ctx, "YYYY-MM-DD"),
            "09.03.24"
        );
        // Unknown keys are left untouched.
        assert_eq!(expand("{{nope}}", "x", ctx, "YYYY-MM-DD"), "{{nope}}");
    }
}
