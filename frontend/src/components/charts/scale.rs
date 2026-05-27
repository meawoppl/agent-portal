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
/// Same enum the backend exposes through the `?bucket=hour|day` query.
/// `Hour` is currently unused by the frontend (the Performance page only
/// requests daily buckets), but the helper supports it so a follow-up
/// hourly-zoom toggle doesn't have to retouch the math.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketKind {
    Hour,
    Day,
}

impl BucketKind {
    /// Parse the wire form (`"hour" | "day"`) into a `BucketKind`. Returns
    /// `None` for any other value; consumers use this to round-trip a server-
    /// chosen bucket from a query-string back into the typed enum. Defensive
    /// fallback to `Day` lives at the call sites.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hour" | "h" => Some(Self::Hour),
            "day" | "d" => Some(Self::Day),
            _ => None,
        }
    }
}

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
/// - `BucketKind::Hour` → `"14:00"` (24-hour HH:MM)
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
            label: format_tick_label(buckets[0], bucket),
        }];
    }
    let n = buckets.len();
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
            label: format_tick_label(buckets[idx], bucket),
        });
    }
    out
}

fn format_tick_label(ts: DateTime<Utc>, bucket: BucketKind) -> String {
    match bucket {
        BucketKind::Hour => format!("{:02}:00", ts.hour()),
        BucketKind::Day => {
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
    }
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

/// Project a series of `(x_index, value)` data points onto an SVG-friendly
/// "x,y x,y …" string for a `<polyline>`. `width` and `height` are the SVG
/// viewBox dimensions; `(y_min, y_max)` are the *displayed* y-axis bounds
/// (typically from [`nice_y_axis`]). Values outside the bounds are clamped.
///
/// Returns an empty string for an empty series; a single-point series renders
/// as one `x,y` pair at `x = width` (right edge), mirroring the Sparkline
/// convention so a one-point chart looks consistent across the page.
pub fn series_points(values: &[f64], width: f32, height: f32, y_min: f64, y_max: f64) -> String {
    if values.is_empty() {
        return String::new();
    }
    if values.len() == 1 {
        let v = values[0];
        let y = value_to_y(v, y_min, y_max, height);
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
        let y = value_to_y(v, y_min, y_max, height);
        out.push_str(&format!("{:.2},{:.2}", x, y));
    }
    out
}

fn value_to_y(v: f64, y_min: f64, y_max: f64, height: f32) -> f32 {
    let span = y_max - y_min;
    if span.abs() < f64::EPSILON {
        return height / 2.0;
    }
    let clamped = v.clamp(y_min, y_max);
    let frac = ((clamped - y_min) / span) as f32;
    // y grows down in SVG, so high values → small y.
    height - frac * height
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
    fn series_points_empty_yields_empty_string() {
        assert_eq!(series_points(&[], 100.0, 50.0, 0.0, 10.0), "");
    }

    #[test]
    fn series_points_single_value_pins_right_edge() {
        let out = series_points(&[5.0], 100.0, 50.0, 0.0, 10.0);
        // single point parks at the right edge (mirrors Sparkline)
        assert!(out.starts_with("100.00,"), "got: {out}");
    }

    #[test]
    fn series_points_endpoints_anchored() {
        let out = series_points(&[0.0, 10.0], 100.0, 50.0, 0.0, 10.0);
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
    fn series_points_clamps_out_of_range() {
        // y_max=10 but value=20 → should clamp to top of chart, not flow off.
        let out = series_points(&[20.0], 100.0, 50.0, 0.0, 10.0);
        let parts: Vec<&str> = out.split_whitespace().collect();
        let y: f32 = parts[0].split(',').nth(1).unwrap().parse().unwrap();
        assert!((y - 0.0).abs() < 0.01);
    }
}
