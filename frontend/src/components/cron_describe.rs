//! Cron expression to English description converter.
//!
//! Supports the standard 5-field UNIX cron format:
//!   min hour dom month dow
//!
//! Field syntax: single values, lists (1,3,5), ranges (1-5), steps (*/15, 1-5/2).

/// Expand a single cron field element (number, range, or range/step) into a sorted
/// list of matching values. Returns None if anything is out of `lo..=hi`.
fn expand_element(elem: &str, lo: u32, hi: u32) -> Option<Vec<u32>> {
    // range/step  e.g. "1-5/2" or "*/15"
    if let Some((range_part, step_str)) = elem.split_once('/') {
        let step: u32 = step_str.parse().ok()?;
        if step == 0 {
            return None;
        }
        let (start, end) = if range_part == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range_part.split_once('-') {
            (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)
        } else {
            let v = range_part.parse::<u32>().ok()?;
            (v, hi)
        };
        if start < lo || end > hi || start > end {
            return None;
        }
        let mut vals = Vec::new();
        let mut v = start;
        while v <= end {
            vals.push(v);
            v += step;
        }
        Some(vals)
    } else if let Some((a, b)) = elem.split_once('-') {
        // plain range e.g. "1-5"
        let start: u32 = a.parse().ok()?;
        let end: u32 = b.parse().ok()?;
        if start < lo || end > hi || start > end {
            return None;
        }
        Some((start..=end).collect())
    } else {
        // single value
        let v: u32 = elem.parse().ok()?;
        if v < lo || v > hi {
            return None;
        }
        Some(vec![v])
    }
}

/// Parse a full cron field (may contain commas) into a sorted, deduplicated list
/// of values, or None for `*` (wildcard).
fn parse_field(field: &str, lo: u32, hi: u32) -> Option<Option<Vec<u32>>> {
    if field == "*" {
        return Some(None); // wildcard
    }
    let mut all = Vec::new();
    for elem in field.split(',') {
        all.extend(expand_element(elem, lo, hi)?);
    }
    if all.is_empty() {
        return None; // invalid
    }
    all.sort_unstable();
    all.dedup();
    Some(Some(all))
}

fn format_hour_min(h: u32, m: u32) -> String {
    let (display_h, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{}:{:02} {}", display_h, m, ampm)
}

fn ordinal(n: u32) -> String {
    let suffix = match n {
        1 | 21 | 31 => "st",
        2 | 22 => "nd",
        3 | 23 => "rd",
        _ => "th",
    };
    format!("{}{}", n, suffix)
}

const DOW_NAMES: [&str; 8] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTH_NAMES: [&str; 13] = [
    "",
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

fn join_english(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} and {}", items[0], items[1]),
        _ => {
            let (last, rest) = items.split_last().unwrap();
            format!("{}, and {}", rest.join(", "), last)
        }
    }
}

/// Describe a 5-field cron expression as a full English sentence.
///
/// Returns `None` if the expression is invalid or has the wrong number of fields.
pub fn describe(expr: &str) -> Option<String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }

    let mins = parse_field(parts[0], 0, 59)?;
    let hours = parse_field(parts[1], 0, 23)?;
    let doms = parse_field(parts[2], 1, 31)?;
    let months = parse_field(parts[3], 1, 12)?;
    let dows = parse_field(parts[4], 0, 7)?;

    // --- Time clause ---
    let time_clause = match (&mins, &hours) {
        (None, None) => "Runs every minute".to_string(),
        (Some(m), None) if m.len() == 1 => {
            format!("Runs at minute {} of every hour", m[0])
        }
        (Some(m), None) => {
            let mins_str: Vec<String> = m.iter().map(|v| format!("{}", v)).collect();
            format!("Runs at minutes {} of every hour", join_english(&mins_str))
        }
        (None, Some(h)) if h.len() == 1 => {
            format!(
                "Runs every minute between {} and {}",
                format_hour_min(h[0], 0),
                format_hour_min(h[0], 59)
            )
        }
        (None, Some(h)) => {
            let hrs: Vec<String> = h.iter().map(|v| format!("{}", v)).collect();
            format!("Runs every minute during hours {}", join_english(&hrs))
        }
        (Some(m), Some(h)) => {
            let times: Vec<String> = h
                .iter()
                .flat_map(|hv| m.iter().map(move |mv| format_hour_min(*hv, *mv)))
                .collect();
            if times.len() == 1 {
                format!("Runs at {}", times[0])
            } else if times.len() <= 6 {
                format!("Runs at {}", join_english(&times))
            } else {
                format!("Runs at {} different times each day", times.len())
            }
        }
    };

    // --- Day-of-week clause ---
    let dow_clause = match &dows {
        None => None,
        Some(d) => {
            let names: Vec<String> = d
                .iter()
                .filter_map(|v| DOW_NAMES.get(*v as usize).map(|s| s.to_string()))
                .collect();
            if names.is_empty() {
                return None;
            }
            Some(format!("on {}", join_english(&names)))
        }
    };

    // --- Day-of-month clause ---
    let dom_clause = match &doms {
        None => None,
        Some(d) => {
            let ords: Vec<String> = d.iter().map(|v| ordinal(*v)).collect();
            if ords.len() == 1 {
                Some(format!("on the {} of the month", ords[0]))
            } else {
                Some(format!("on the {} of the month", join_english(&ords)))
            }
        }
    };

    // --- Month clause ---
    let month_clause = match &months {
        None => None,
        Some(m) => {
            let names: Vec<String> = m
                .iter()
                .filter_map(|v| MONTH_NAMES.get(*v as usize).map(|s| s.to_string()))
                .collect();
            if names.is_empty() {
                return None;
            }
            Some(format!("in {}", join_english(&names)))
        }
    };

    // --- Assemble sentence ---
    let mut sentence = time_clause;

    // When both dom and dow are specified, cron fires when EITHER matches
    match (&dom_clause, &dow_clause) {
        (Some(dom), Some(dow)) => {
            sentence = format!("{}, {} or {}", sentence, dom, dow);
        }
        (Some(dom), None) => {
            sentence = format!("{}, {}", sentence, dom);
        }
        (None, Some(dow)) => {
            sentence = format!("{}, {}", sentence, dow);
        }
        (None, None) => {}
    }

    if let Some(mc) = month_clause {
        sentence = format!("{}, {}", sentence, mc);
    }

    sentence.push('.');
    Some(sentence)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- expand_element ---

    #[test]
    fn single_value() {
        assert_eq!(expand_element("5", 0, 59), Some(vec![5]));
        assert_eq!(expand_element("0", 0, 23), Some(vec![0]));
    }

    #[test]
    fn out_of_range() {
        assert_eq!(expand_element("60", 0, 59), None);
        assert_eq!(expand_element("32", 1, 31), None);
    }

    #[test]
    fn range() {
        assert_eq!(expand_element("1-5", 0, 59), Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(expand_element("10-12", 1, 31), Some(vec![10, 11, 12]));
    }

    #[test]
    fn range_with_step() {
        assert_eq!(expand_element("0-10/3", 0, 59), Some(vec![0, 3, 6, 9]));
        assert_eq!(expand_element("1-7/2", 1, 31), Some(vec![1, 3, 5, 7]));
    }

    #[test]
    fn star_step() {
        assert_eq!(expand_element("*/15", 0, 59), Some(vec![0, 15, 30, 45]));
        assert_eq!(expand_element("*/6", 0, 23), Some(vec![0, 6, 12, 18]));
    }

    #[test]
    fn step_zero_invalid() {
        assert_eq!(expand_element("*/0", 0, 59), None);
    }

    #[test]
    fn reversed_range_invalid() {
        assert_eq!(expand_element("10-5", 0, 59), None);
    }

    // --- parse_field ---

    #[test]
    fn wildcard() {
        assert_eq!(parse_field("*", 0, 59), Some(None));
    }

    #[test]
    fn list() {
        assert_eq!(parse_field("1,3,5", 0, 59), Some(Some(vec![1, 3, 5])));
    }

    #[test]
    fn list_with_ranges() {
        assert_eq!(
            parse_field("1-3,7,10-12", 0, 59),
            Some(Some(vec![1, 2, 3, 7, 10, 11, 12]))
        );
    }

    #[test]
    fn list_deduplicates() {
        assert_eq!(parse_field("1,1,2,2", 0, 59), Some(Some(vec![1, 2])));
    }

    #[test]
    fn invalid_element_in_list() {
        assert_eq!(parse_field("1,99", 0, 59), None);
    }

    // --- describe: full sentences ---

    #[test]
    fn every_minute() {
        assert_eq!(describe("* * * * *"), Some("Runs every minute.".into()));
    }

    #[test]
    fn specific_time_daily() {
        assert_eq!(describe("0 3 * * *"), Some("Runs at 3:00 AM.".into()));
    }

    #[test]
    fn specific_time_pm() {
        assert_eq!(describe("30 14 * * *"), Some("Runs at 2:30 PM.".into()));
    }

    #[test]
    fn midnight() {
        assert_eq!(describe("0 0 * * *"), Some("Runs at 12:00 AM.".into()));
    }

    #[test]
    fn noon() {
        assert_eq!(describe("0 12 * * *"), Some("Runs at 12:00 PM.".into()));
    }

    #[test]
    fn every_15_minutes() {
        assert_eq!(
            describe("*/15 * * * *"),
            Some("Runs at minutes 0, 15, 30, and 45 of every hour.".into())
        );
    }

    #[test]
    fn weekdays_only() {
        assert_eq!(
            describe("0 9 * * 1-5"),
            Some("Runs at 9:00 AM, on Monday, Tuesday, Wednesday, Thursday, and Friday.".into())
        );
    }

    #[test]
    fn specific_dow_list() {
        assert_eq!(
            describe("0 9 * * 1,3,5"),
            Some("Runs at 9:00 AM, on Monday, Wednesday, and Friday.".into())
        );
    }

    #[test]
    fn monthly_on_first() {
        assert_eq!(
            describe("0 6 1 * *"),
            Some("Runs at 6:00 AM, on the 1st of the month.".into())
        );
    }

    #[test]
    fn specific_months() {
        assert_eq!(
            describe("0 8 1 1,6 *"),
            Some("Runs at 8:00 AM, on the 1st of the month, in January and June.".into())
        );
    }

    #[test]
    fn dom_and_dow_both_set() {
        assert_eq!(
            describe("0 9 15 * 1"),
            Some("Runs at 9:00 AM, on the 15th of the month or on Monday.".into())
        );
    }

    #[test]
    fn multiple_times() {
        assert_eq!(
            describe("0,30 8,17 * * *"),
            Some("Runs at 8:00 AM, 8:30 AM, 5:00 PM, and 5:30 PM.".into())
        );
    }

    #[test]
    fn step_hours() {
        assert_eq!(
            describe("0 */6 * * *"),
            Some("Runs at 12:00 AM, 6:00 AM, 12:00 PM, and 6:00 PM.".into())
        );
    }

    #[test]
    fn multiple_dom() {
        assert_eq!(
            describe("0 9 1,15 * *"),
            Some("Runs at 9:00 AM, on the 1st and 15th of the month.".into())
        );
    }

    #[test]
    fn complex_range_list() {
        assert_eq!(
            describe("0 9 * * 1-3,5"),
            Some("Runs at 9:00 AM, on Monday, Tuesday, Wednesday, and Friday.".into())
        );
    }

    #[test]
    fn every_hour_at_specific_minute() {
        assert_eq!(
            describe("30 * * * *"),
            Some("Runs at minute 30 of every hour.".into())
        );
    }

    #[test]
    fn sunday_both_0_and_7() {
        assert_eq!(
            describe("0 9 * * 0"),
            Some("Runs at 9:00 AM, on Sunday.".into())
        );
        assert_eq!(
            describe("0 9 * * 7"),
            Some("Runs at 9:00 AM, on Sunday.".into())
        );
    }

    #[test]
    fn invalid_too_few_fields() {
        assert_eq!(describe("* * *"), None);
    }

    #[test]
    fn invalid_too_many_fields() {
        assert_eq!(describe("* * * * * *"), None);
    }

    #[test]
    fn invalid_out_of_range() {
        assert_eq!(describe("60 * * * *"), None);
        assert_eq!(describe("* 25 * * *"), None);
        assert_eq!(describe("* * 32 * *"), None);
        assert_eq!(describe("* * * 13 *"), None);
        assert_eq!(describe("* * * * 8"), None);
    }

    #[test]
    fn all_months() {
        let desc = describe("0 0 1 1,2,3,4,5,6,7,8,9,10,11,12 *").unwrap();
        assert!(desc.starts_with("Runs at 12:00 AM"));
        assert!(desc.contains("January"));
        assert!(desc.contains("December"));
    }

    // --- helper functions ---

    #[test]
    fn test_format_hour_min() {
        assert_eq!(format_hour_min(0, 0), "12:00 AM");
        assert_eq!(format_hour_min(9, 5), "9:05 AM");
        assert_eq!(format_hour_min(12, 0), "12:00 PM");
        assert_eq!(format_hour_min(13, 30), "1:30 PM");
        assert_eq!(format_hour_min(23, 59), "11:59 PM");
    }

    #[test]
    fn test_ordinal() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(23), "23rd");
        assert_eq!(ordinal(31), "31st");
    }

    #[test]
    fn test_join_english() {
        let s = |v: &str| v.to_string();
        assert_eq!(join_english(&[]), "");
        assert_eq!(join_english(&[s("Mon")]), "Mon");
        assert_eq!(join_english(&[s("Mon"), s("Fri")]), "Mon and Fri");
        assert_eq!(
            join_english(&[s("Mon"), s("Wed"), s("Fri")]),
            "Mon, Wed, and Fri"
        );
    }
}
