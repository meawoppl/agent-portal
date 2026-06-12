//! Pure axis / scale math for the Performance page's hand-rolled SVG charts.
//!
//! Every function in this module is pure — no DOM, no Yew, no chrono parsing
//! that depends on the browser locale. Each chart in the parent module pulls
//! these helpers to convert data values into SVG `x`/`y` coordinates and
//! "nice" axis tick positions.
//!
//! Kept in its own file so the math can be unit-tested independently. The
//! rendered SVG is *not* tested; the polyline-point string is generated from
//! [`series_points`] in the chart component itself.

use chrono::{DateTime, Datelike, Timelike, Utc};

/// Bucket granularity, used to pick a date / time format for axis tick labels.
/// Same enum shape the backend exposes through the `?bucket=…` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketKind {
    Minute,
    Hour,
    Day,
}

impl BucketKind {
    /// Parse the wire form into a `BucketKind`. Returns
    /// `None` for any other value; consumers use this to round-trip a server-
    /// chosen bucket from a query-string back into the typed enum. Defensive
    /// fallback to `Day` lives at the call sites.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minute" | "m" | "1m" | "5m" | "15m" => Some(Self::Minute),
            "hour" | "h" => Some(Self::Hour),
            "day" | "d" => Some(Self::Day),
            _ => None,
        }
    }
}

/// Y-axis projection mode for Performance charts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisScale {
    Linear,
    Log,
}

impl AxisScale {
    pub const fn all() -> [Self; 2] {
        [Self::Linear, Self::Log]
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Log => "Log",
        }
    }
}

/// Complete y-axis mapping, including displayed tick positions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum YAxis {
    Linear { min: f64, max: f64, step: f64 },
    Log { min_log: f64, max_log: f64 },
}

const LOG_ZERO_BAND_FRAC: f32 = 0.08;

/// One axis tick — a normalized x position in `[0.0, 1.0]` plus the label
/// string to render. Consumers multiply by the chart width to get the SVG
/// `x` coordinate.
#[derive(Debug, Clone, PartialEq)]
pub struct TickLabel {
    pub frac: f32,
    pub label: String,
}

/// Compute "nice" y-axis bounds + tick step for a data range.
///
/// Returns `(nice_min, nice_max, step)` where `nice_max - nice_min` is a
/// round multiple of `step`. `step` is chosen so the chart has roughly 4–6
/// tick labels. Implements the "1/2/5 times a power of ten" heuristic — the
/// same trick D3 / Matplotlib / gnuplot all use.
///
/// Edge cases:
/// - `min == max` returns `(min - 0.5, max + 0.5, 0.25)` so a flat run still
///   renders a visible band.
/// - `min < 0` is clamped to `0` for the lower bound (every metric on the
///   Performance page is non-negative — tok/s, TTFT, cache hit, cost).
pub fn nice_y_axis(min: f64, max: f64) -> (f64, f64, f64) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0, 0.25);
    }
    if (max - min).abs() < f64::EPSILON {
        // Flat — render a 1-unit band centered on the value, clamped to ≥ 0.
        let lo = (min - 0.5).max(0.0);
        let hi = max + 0.5;
        return (lo, hi, 0.25);
    }
    let span = max - min;
    // Aim for ~5 ticks, so step ≈ span / 5; pick a 1/2/5 × 10^n that's close.
    let rough_step = span / 5.0;
    let step = nice_step(rough_step);
    let nice_min = (min / step).floor() * step;
    let nice_min = nice_min.max(0.0); // never below zero for our metrics
    let nice_max = (max / step).ceil() * step;
    (nice_min, nice_max, step)
}

/// Build a linear or logarithmic y-axis from the finite chart values.
///
/// Log scale reserves a small baseline band for zero/non-positive values, then
/// maps positive values by base-10 decades. That keeps sparse zero buckets
/// visible without pretending that `log(0)` exists.
pub fn y_axis_for_values(values: &[f64], scale: AxisScale) -> YAxis {
    match scale {
        AxisScale::Linear => {
            let (min_v, max_v) = finite_min_max(values).unwrap_or((0.0, 1.0));
            let (min, max, step) = nice_y_axis(min_v, max_v);
            YAxis::Linear { min, max, step }
        }
        AxisScale::Log => {
            let positives: Vec<f64> = values
                .iter()
                .copied()
                .filter(|v| v.is_finite() && *v > 0.0)
                .collect();
            let Some((min_positive, max_positive)) = finite_min_max(&positives) else {
                let (min, max, step) = nice_y_axis(0.0, 1.0);
                return YAxis::Linear { min, max, step };
            };
            let mut min_log = min_positive.log10().floor();
            let mut max_log = max_positive.log10().ceil();
            if (max_log - min_log).abs() < f64::EPSILON {
                min_log -= 1.0;
                max_log += 1.0;
            }
            YAxis::Log { min_log, max_log }
        }
    }
}

fn finite_min_max(values: &[f64]) -> Option<(f64, f64)> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| {
            Some(match acc {
                Some((min, max)) => (min.min(v), max.max(v)),
                None => (v, v),
            })
        })
}

/// Round `raw` up to the nearest "nice" value (1, 2, or 5 times a power of ten).
///
/// Examples: `nice_step(1.3) → 2.0`, `nice_step(4.7) → 5.0`,
/// `nice_step(73.2) → 100.0`, `nice_step(0.03) → 0.05`.
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return 1.0;
    }
    let exp = raw.log10().floor();
    let pow = 10f64.powf(exp);
    let frac = raw / pow;
    let nice = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * pow
}

/// Build evenly-spaced x-axis tick labels for the bucket timeline.
///
/// `buckets` is the ordered list of bucket-start timestamps; the function
/// picks at most `max_ticks` of them (default behavior: ~6 ticks even for a
/// 90-day range), normalized to `[0.0, 1.0]` along the x axis.
///
/// Labels:
/// - `BucketKind::Day` → `"May 5"` (month abbreviation + day-of-month)
/// - single-day `BucketKind::Minute` / `Hour` → `"14:05"` / `"14:00"`
/// - multi-day `BucketKind::Minute` / `Hour` → `"May 5 14:05"` / `"May 5 14:00"`
pub fn time_axis_ticks(
    buckets: &[DateTime<Utc>],
    bucket: BucketKind,
    max_ticks: usize,
) -> Vec<TickLabel> {
    if buckets.is_empty() {
        return Vec::new();
    }
    if buckets.len() == 1 {
        return vec![TickLabel {
            frac: 0.5,
            label: format_tick_label(buckets[0], bucket, false),
        }];
    }
    let n = buckets.len();
    let include_date = matches!(bucket, BucketKind::Minute | BucketKind::Hour)
        && buckets
            .first()
            .zip(buckets.last())
            .is_some_and(|(first, last)| first.date_naive() != last.date_naive());
    let target = max_ticks.clamp(2, n);
    // Step so we land on `target - 1` intervals across the [0..n-1] range.
    let last = (n - 1) as f64;
    let step = last / (target as f64 - 1.0);
    let mut out = Vec::with_capacity(target);
    for i in 0..target {
        let idx = (i as f64 * step).round() as usize;
        let idx = idx.min(n - 1);
        let frac = if n == 1 {
            0.5
        } else {
            (idx as f32) / (last as f32)
        };
        out.push(TickLabel {
            frac,
            label: format_tick_label(buckets[idx], bucket, include_date),
        });
    }
    out
}

fn format_tick_label(ts: DateTime<Utc>, bucket: BucketKind, include_date: bool) -> String {
    match bucket {
        BucketKind::Minute => {
            let time = format!("{:02}:{:02}", ts.hour(), ts.minute());
            if include_date {
                format!("{} {time}", format_month_day(ts))
            } else {
                time
            }
        }
        BucketKind::Hour => {
            let time = format!("{:02}:00", ts.hour());
            if include_date {
                format!("{} {time}", format_month_day(ts))
            } else {
                time
            }
        }
        BucketKind::Day => format_month_day(ts),
    }
}

fn format_month_day(ts: DateTime<Utc>) -> String {
    let month = match ts.month() {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "?",
    };
    format!("{} {}", month, ts.day())
}

/// Build the y-axis ticks as `(value, normalized_frac)` pairs. `frac == 0.0`
/// is the bottom of the chart, `frac == 1.0` is the top. Useful for emitting
/// horizontal gridlines and tick labels with the same axis math.
pub fn y_axis_ticks(min: f64, max: f64, step: f64) -> Vec<(f64, f32)> {
    if step <= 0.0 || !step.is_finite() || max <= min {
        return Vec::new();
    }
    let span = max - min;
    let mut out = Vec::new();
    let mut v = min;
    // Allow a tiny epsilon so floating-point drift in `nice_y_axis` doesn't
    // drop the top tick.
    let epsilon = step * 1e-6;
    while v <= max + epsilon {
        let frac = ((v - min) / span) as f32;
        out.push((v, frac));
        v += step;
    }
    out
}

/// Build y-axis ticks for an already resolved axis mapping.
pub fn y_axis_tick_labels(axis: &YAxis) -> Vec<(f64, f32)> {
    match *axis {
        YAxis::Linear { min, max, step } => y_axis_ticks(min, max, step),
        YAxis::Log { min_log, max_log } => {
            if !min_log.is_finite() || !max_log.is_finite() || max_log <= min_log {
                return Vec::new();
            }
            let mut ticks = vec![(0.0, 0.0)];
            let min_power = min_log as i32;
            let max_power = max_log as i32;
            for power in min_power..=max_power {
                let value = 10f64.powi(power);
                ticks.push((value, value_frac(value, axis)));
            }
            ticks
        }
    }
}

/// Project a series using a resolved [`YAxis`] mapping.
///
/// Returns an empty string for an empty series; a single-point series renders
/// as one `x,y` pair at `x = width` (right edge), mirroring the Sparkline
/// convention so a one-point chart looks consistent across the page.
pub fn series_points_with_axis(values: &[f64], width: f32, height: f32, axis: &YAxis) -> String {
    if values.is_empty() {
        return String::new();
    }
    if values.len() == 1 {
        let v = values[0];
        let y = value_to_y(v, axis, height);
        return format!("{:.2},{:.2}", width, y);
    }
    let n = values.len();
    let step = width / (n as f32 - 1.0);
    let mut out = String::with_capacity(n * 12);
    for (i, &v) in values.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let x = step * i as f32;
        let y = value_to_y(v, axis, height);
        out.push_str(&format!("{:.2},{:.2}", x, y));
    }
    out
}

/// Convert a raw value to an SVG y coordinate for `height`.
pub fn value_to_y(v: f64, axis: &YAxis, height: f32) -> f32 {
    let frac = value_frac(v, axis);
    height - frac * height
}

/// Convert a raw value to a normalized y fraction in `[0.0, 1.0]`.
pub fn value_frac(v: f64, axis: &YAxis) -> f32 {
    match *axis {
        YAxis::Linear { min, max, .. } => {
            let span = max - min;
            if span.abs() < f64::EPSILON {
                return 0.5;
            }
            let clamped = v.clamp(min, max);
            ((clamped - min) / span) as f32
        }
        YAxis::Log { min_log, max_log } => {
            if !v.is_finite() || v <= 0.0 {
                return 0.0;
            }
            let span = max_log - min_log;
            if span.abs() < f64::EPSILON {
                return 1.0;
            }
            let log_frac = ((v.log10() - min_log) / span).clamp(0.0, 1.0) as f32;
            LOG_ZERO_BAND_FRAC + log_frac * (1.0 - LOG_ZERO_BAND_FRAC)
        }
    }
}

/// Format a y-axis tick value as compactly as possible. Whole numbers drop the
/// decimal; small fractions keep enough precision to avoid rendering as `0`.
pub fn format_axis_value(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v.abs() >= 1000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else if v.abs() >= 1.0 {
        format!("{:.1}", v)
    } else if v.abs() >= 0.01 {
        format!("{:.2}", v)
    } else if v.abs() >= 0.001 {
        format!("{:.3}", v)
    } else {
        format!("{:.1e}", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn nice_step_rounds_up_to_1_2_5_decade() {
        assert!((nice_step(1.0) - 1.0).abs() < 1e-9);
        assert!((nice_step(1.3) - 2.0).abs() < 1e-9);
        assert!((nice_step(4.7) - 5.0).abs() < 1e-9);
        assert!((nice_step(6.5) - 10.0).abs() < 1e-9);
        assert!((nice_step(73.2) - 100.0).abs() < 1e-9);
        assert!((nice_step(0.03) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn nice_y_axis_typical_range() {
        // tok/s p50 over a few buckets — say 12.0 to 47.5
        let (lo, hi, step) = nice_y_axis(12.0, 47.5);
        assert_eq!(lo, 10.0);
        assert_eq!(hi, 50.0);
        assert_eq!(step, 10.0);
    }

    #[test]
    fn nice_y_axis_small_range() {
        // ttft in seconds — 0.4 to 1.2
        let (lo, hi, step) = nice_y_axis(0.4, 1.2);
        // step rough = 0.16 → nice 0.2; bounds snap to [0.4, 1.2] nearest 0.2.
        assert!((step - 0.2).abs() < 1e-9);
        assert!(lo <= 0.4);
        assert!(hi >= 1.2);
    }

    #[test]
    fn nice_y_axis_flat_run_pads_visibly() {
        // All values equal — still render a band, never collapse to zero width.
        let (lo, hi, step) = nice_y_axis(5.0, 5.0);
        assert!(hi > lo);
        assert!(step > 0.0);
    }

    #[test]
    fn nice_y_axis_clamps_negative_lower_bound_to_zero() {
        // Should never produce a negative axis for our non-negative metrics.
        let (lo, _hi, _step) = nice_y_axis(0.5, 3.0);
        assert!(lo >= 0.0);
    }

    #[test]
    fn nice_y_axis_handles_nan() {
        // Defensive — NaN should not panic.
        let (lo, hi, step) = nice_y_axis(f64::NAN, 1.0);
        assert!(hi > lo);
        assert!(step > 0.0);
    }

    #[test]
    fn time_axis_ticks_empty() {
        assert!(time_axis_ticks(&[], BucketKind::Day, 6).is_empty());
    }

    #[test]
    fn time_axis_ticks_single_bucket_renders_one_centered_tick() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 5, 12, 0, 0).unwrap();
        let ticks = time_axis_ticks(&[ts], BucketKind::Day, 6);
        assert_eq!(ticks.len(), 1);
        assert!((ticks[0].frac - 0.5).abs() < 1e-6);
        assert_eq!(ticks[0].label, "May 5");
    }

    #[test]
    fn time_axis_ticks_daily_labels_format_as_month_day() {
        // 30 days, ask for 5 ticks → should land on day 0, ~7, ~15, ~22, 29.
        let mut buckets = Vec::new();
        for day in 1..=30 {
            buckets.push(Utc.with_ymd_and_hms(2026, 5, day, 0, 0, 0).unwrap());
        }
        let ticks = time_axis_ticks(&buckets, BucketKind::Day, 5);
        assert_eq!(ticks.len(), 5);
        // First tick at the start, last tick at the end.
        assert!((ticks[0].frac - 0.0).abs() < 1e-6);
        assert!((ticks[4].frac - 1.0).abs() < 1e-6);
        assert_eq!(ticks[0].label, "May 1");
        assert_eq!(ticks[4].label, "May 30");
        // Middle tick is roughly at day 15.
        assert!(ticks[2].label.starts_with("May"));
    }

    #[test]
    fn time_axis_ticks_hourly_labels_format_as_hh_colon_zero_zero() {
        let mut buckets = Vec::new();
        for hour in 0..24 {
            buckets.push(Utc.with_ymd_and_hms(2026, 5, 5, hour, 0, 0).unwrap());
        }
        let ticks = time_axis_ticks(&buckets, BucketKind::Hour, 4);
        assert_eq!(ticks.len(), 4);
        assert_eq!(ticks[0].label, "00:00");
        assert_eq!(ticks[3].label, "23:00");
    }

    #[test]
    fn time_axis_ticks_hourly_labels_include_dates_across_days() {
        let mut buckets = Vec::new();
        for day in 5..=7 {
            for hour in 0..24 {
                buckets.push(Utc.with_ymd_and_hms(2026, 5, day, hour, 0, 0).unwrap());
            }
        }
        let ticks = time_axis_ticks(&buckets, BucketKind::Hour, 4);
        assert_eq!(ticks.len(), 4);
        assert_eq!(ticks[0].label, "May 5 00:00");
        assert_eq!(ticks[3].label, "May 7 23:00");
    }

    #[test]
    fn time_axis_ticks_minute_labels_include_minutes() {
        let mut buckets = Vec::new();
        for minute in 0..60 {
            buckets.push(Utc.with_ymd_and_hms(2026, 5, 5, 14, minute, 0).unwrap());
        }
        let ticks = time_axis_ticks(&buckets, BucketKind::Minute, 5);
        assert_eq!(ticks.len(), 5);
        assert_eq!(ticks[0].label, "14:00");
        assert_eq!(ticks[4].label, "14:59");
    }

    #[test]
    fn time_axis_ticks_minute_labels_include_dates_across_days() {
        let buckets = vec![
            Utc.with_ymd_and_hms(2026, 5, 5, 23, 55, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 6, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 6, 0, 5, 0).unwrap(),
        ];
        let ticks = time_axis_ticks(&buckets, BucketKind::Minute, 3);
        assert_eq!(ticks[0].label, "May 5 23:55");
        assert_eq!(ticks[2].label, "May 6 00:05");
    }

    #[test]
    fn time_axis_ticks_caps_at_bucket_count() {
        // Ask for more ticks than buckets — we should never return more than
        // `buckets.len()`.
        let buckets = vec![Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()];
        let ticks = time_axis_ticks(&buckets, BucketKind::Day, 10);
        assert_eq!(ticks.len(), 1);
    }

    #[test]
    fn y_axis_ticks_evenly_spaced() {
        let ticks = y_axis_ticks(0.0, 10.0, 2.5);
        assert_eq!(ticks.len(), 5);
        assert!((ticks[0].0 - 0.0).abs() < 1e-9);
        assert!((ticks[4].0 - 10.0).abs() < 1e-9);
        assert!((ticks[0].1 - 0.0).abs() < 1e-6);
        assert!((ticks[4].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn y_axis_ticks_invalid_step_returns_empty() {
        assert!(y_axis_ticks(0.0, 10.0, 0.0).is_empty());
        assert!(y_axis_ticks(10.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn log_y_axis_reserves_zero_baseline() {
        let axis = y_axis_for_values(&[0.0, 1.0, 10.0, 100.0], AxisScale::Log);
        let ticks = y_axis_tick_labels(&axis);
        assert_eq!(ticks[0], (0.0, 0.0));
        assert_eq!(value_frac(0.0, &axis), 0.0);
        assert!(value_frac(1.0, &axis) > 0.0);
        assert!(value_frac(100.0, &axis) > value_frac(10.0, &axis));
    }

    #[test]
    fn log_y_axis_without_positive_values_falls_back_to_linear() {
        let axis = y_axis_for_values(&[0.0, 0.0], AxisScale::Log);
        assert!(matches!(axis, YAxis::Linear { .. }));
    }

    #[test]
    fn series_points_empty_yields_empty_string() {
        let axis = YAxis::Linear {
            min: 0.0,
            max: 10.0,
            step: 1.0,
        };
        assert_eq!(series_points_with_axis(&[], 100.0, 50.0, &axis), "");
    }

    #[test]
    fn series_points_single_value_pins_right_edge() {
        let axis = YAxis::Linear {
            min: 0.0,
            max: 10.0,
            step: 1.0,
        };
        let out = series_points_with_axis(&[5.0], 100.0, 50.0, &axis);
        // single point parks at the right edge (mirrors Sparkline)
        assert!(out.starts_with("100.00,"), "got: {out}");
    }

    #[test]
    fn series_points_endpoints_anchored() {
        let axis = YAxis::Linear {
            min: 0.0,
            max: 10.0,
            step: 1.0,
        };
        let out = series_points_with_axis(&[0.0, 10.0], 100.0, 50.0, &axis);
        let parts: Vec<&str> = out.split_whitespace().collect();
        assert_eq!(parts.len(), 2);
        // First x is 0, last x is 100.
        assert!(parts[0].starts_with("0.00,"));
        assert!(parts[1].starts_with("100.00,"));
        // y-axis is inverted (SVG y grows down): value 0 → y=height; value 10 → y=0.
        let y0: f32 = parts[0].split(',').nth(1).unwrap().parse().unwrap();
        let y1: f32 = parts[1].split(',').nth(1).unwrap().parse().unwrap();
        assert!((y0 - 50.0).abs() < 0.01);
        assert!((y1 - 0.0).abs() < 0.01);
    }

    #[test]
    fn format_axis_value_picks_compact_form() {
        assert_eq!(format_axis_value(0.0), "0");
        assert_eq!(format_axis_value(1.0), "1");
        assert_eq!(format_axis_value(5.0), "5");
        assert_eq!(format_axis_value(0.25), "0.25");
        assert_eq!(format_axis_value(7.5), "7.5");
        assert_eq!(format_axis_value(12.5), "12.5");
        assert_eq!(format_axis_value(1500.0), "1.5k");
        assert_eq!(format_axis_value(2500.0), "2.5k");
    }

    #[test]
    fn series_points_clamps_out_of_range() {
        // y_max=10 but value=20 → should clamp to top of chart, not flow off.
        let axis = YAxis::Linear {
            min: 0.0,
            max: 10.0,
            step: 1.0,
        };
        let out = series_points_with_axis(&[20.0], 100.0, 50.0, &axis);
        let parts: Vec<&str> = out.split_whitespace().collect();
        let y: f32 = parts[0].split(',').nth(1).unwrap().parse().unwrap();
        assert!((y - 0.0).abs() < 0.01);
    }
}
