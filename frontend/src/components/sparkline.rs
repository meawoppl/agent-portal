//! Pure-SVG sparkline component used by the dashboard-header
//! [`TurnMetricsHeaderPill`](super::turn_metrics_pill::TurnMetricsHeaderPill).
//!
//! Hand-rolled — no chart library dependency — so the dashboard pill stays
//! a single `<svg>` of a few dozen bytes and the project doesn't acquire a
//! new WASM-incompatible-by-default chart crate. The visual is one
//! `<polyline>` plus a single highlighted `<circle>` on the most-recent
//! point; auto-scales the y-axis to the data's min/max with a small visual
//! margin so a flat run doesn't draw at the very top/bottom edge.
//!
//! All point-projection math lives in [`points_attr`], a pure helper, so the
//! shape can be unit-tested against expected SVG `points="…"` strings
//! without spinning up Yew.

use yew::prelude::*;

/// Tokyo-Night accent blue — matches the per-turn footer chip color and the
/// pill border. Used for both the polyline stroke and the most-recent-point
/// dot.
const ACCENT_COLOR: &str = "#7aa2f7";

/// Fractional vertical margin added inside the SVG viewbox so the polyline
/// doesn't render flush against the top/bottom edges. With a 20px height, a
/// 0.15 margin leaves 3px of breathing room on each side.
const Y_MARGIN_FRAC: f32 = 0.15;

/// Build the `points="x1,y1 x2,y2 …"` attribute string for the polyline.
///
/// Coordinate system: SVG y grows downward, so `min` of the data maps to the
/// *largest* y (bottom of the viewbox) and `max` maps to the smallest y
/// (top). When all values are equal — or there's exactly one point — y is
/// pinned to the vertical center.
///
/// Edge cases:
/// - **Empty input**: returns an empty string. The polyline element with
///   `points=""` is a no-op, and the renderer also short-circuits to a flat
///   dashed midline for this case.
/// - **Single point**: emits exactly one `"x,y"` pair at the right edge of
///   the viewbox so the "most-recent dot" lines up with where it would be
///   in a multi-point series.
/// - **Many points**: equally-spaced along the x axis from 0 to `width`.
pub fn points_attr(values: &[f64], width: f32, height: f32) -> String {
    if values.is_empty() {
        return String::new();
    }

    let y_margin = height * Y_MARGIN_FRAC;
    let y_top = y_margin;
    let y_bot = height - y_margin;
    let y_mid = height / 2.0;

    if values.len() == 1 {
        // Single point — pin to the right edge at the vertical midpoint so
        // it visually matches where the "current value" dot lands in the
        // multi-point case.
        return format!("{:.2},{:.2}", width, y_mid);
    }

    let (min_v, max_v) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    let span = max_v - min_v;

    let n = values.len();
    let step = if n > 1 { width / (n as f32 - 1.0) } else { 0.0 };

    let mut out = String::with_capacity(n * 12);
    for (i, &v) in values.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let x = step * i as f32;
        // Map data → y. Flat run (span == 0) parks the line at the vertical
        // midpoint so it doesn't degenerate to a NaN or sit on an edge.
        let y = if span.abs() < f64::EPSILON {
            y_mid
        } else {
            let frac = ((v - min_v) / span) as f32;
            // High values → smaller y (top); low values → larger y (bottom).
            y_bot - frac * (y_bot - y_top)
        };
        // Two-decimal precision keeps the attr string compact while staying
        // sub-pixel-accurate at the typical 80×20 pill size.
        out.push_str(&format!("{:.2},{:.2}", x, y));
    }
    out
}

/// Extract the last `"x,y"` coordinate pair from a `points_attr` result.
/// Returns `None` for empty input. Used by the renderer to position the
/// most-recent-point dot.
fn last_point(points: &str) -> Option<(f32, f32)> {
    let last = points.split_whitespace().next_back()?;
    let mut parts = last.split(',');
    let x: f32 = parts.next()?.parse().ok()?;
    let y: f32 = parts.next()?.parse().ok()?;
    Some((x, y))
}

#[derive(Properties, PartialEq)]
pub struct SparklineProps {
    /// Data points in oldest → newest order. Empty input renders a flat
    /// dashed midline (the "no data" placeholder); single-point input
    /// renders just the dot.
    pub values: Vec<f64>,
    /// Width of the SVG in CSS pixels. Defaults to 80.
    #[prop_or(80.0)]
    pub width: f32,
    /// Height of the SVG in CSS pixels. Defaults to 20.
    #[prop_or(20.0)]
    pub height: f32,
}

#[function_component(Sparkline)]
pub fn sparkline(props: &SparklineProps) -> Html {
    let width = props.width;
    let height = props.height;
    let viewbox = format!("0 0 {} {}", width, height);

    if props.values.is_empty() {
        // Empty state: a thin dashed midline so the pill keeps its layout
        // (the parent decides whether to render the pill at all — see
        // TurnMetricsHeaderPill).
        let mid_y = height / 2.0;
        return html! {
            <svg
                class="sparkline"
                width={width.to_string()}
                height={height.to_string()}
                viewBox={viewbox}
                aria-hidden="true"
            >
                <line
                    x1="0"
                    y1={mid_y.to_string()}
                    x2={width.to_string()}
                    y2={mid_y.to_string()}
                    stroke={ACCENT_COLOR}
                    stroke-opacity="0.35"
                    stroke-dasharray="2,2"
                    stroke-width="1"
                />
            </svg>
        };
    }

    let points = points_attr(&props.values, width, height);
    let last = last_point(&points);

    html! {
        <svg
            class="sparkline"
            width={width.to_string()}
            height={height.to_string()}
            viewBox={viewbox}
            aria-hidden="true"
        >
            { if props.values.len() >= 2 {
                html! {
                    <polyline
                        fill="none"
                        stroke={ACCENT_COLOR}
                        stroke-width="1.5"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        points={points.clone()}
                    />
                }
            } else {
                html! {}
            } }
            { if let Some((cx, cy)) = last {
                html! {
                    <circle
                        cx={format!("{:.2}", cx)}
                        cy={format!("{:.2}", cy)}
                        r="1.75"
                        fill={ACCENT_COLOR}
                    />
                }
            } else {
                html! {}
            } }
        </svg>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_attr_empty() {
        // Empty input → empty string. The renderer short-circuits to a
        // dashed midline so the pill keeps its layout.
        assert_eq!(points_attr(&[], 80.0, 20.0), "");
    }

    #[test]
    fn points_attr_single() {
        // Single point: parked at the right edge, vertical center.
        // 80x20 box → center y is 10.0.
        let out = points_attr(&[42.0], 80.0, 20.0);
        assert_eq!(out, "80.00,10.00");
    }

    #[test]
    fn points_attr_two_points_endpoints_correct() {
        // Two points: first at x=0, last at x=width. Larger value should
        // map to a smaller y (top of the SVG), per the y-grows-down
        // convention.
        let out = points_attr(&[1.0, 2.0], 80.0, 20.0);
        let coords: Vec<&str> = out.split_whitespace().collect();
        assert_eq!(coords.len(), 2);
        // x positions:
        assert!(
            coords[0].starts_with("0.00,"),
            "first x should be 0, got: {}",
            coords[0]
        );
        assert!(
            coords[1].starts_with("80.00,"),
            "last x should be 80, got: {}",
            coords[1]
        );
        // y positions: index 0 is the smaller value (1.0) → larger y;
        // index 1 is the larger value (2.0) → smaller y.
        let y0: f32 = coords[0].split(',').nth(1).unwrap().parse().unwrap();
        let y1: f32 = coords[1].split(',').nth(1).unwrap().parse().unwrap();
        assert!(
            y0 > y1,
            "smaller value should have larger y: y0={y0} y1={y1}"
        );
    }

    #[test]
    fn points_attr_flat_run_parks_at_center() {
        // All values equal — should park every y at the vertical midpoint
        // so the line doesn't degenerate to NaN or sit on an edge.
        let out = points_attr(&[5.0, 5.0, 5.0], 80.0, 20.0);
        let coords: Vec<&str> = out.split_whitespace().collect();
        assert_eq!(coords.len(), 3);
        for c in &coords {
            let y: f32 = c.split(',').nth(1).unwrap().parse().unwrap();
            assert!(
                (y - 10.0).abs() < 0.01,
                "flat run y should be 10.0, got: {y}"
            );
        }
    }

    #[test]
    fn points_attr_many_points_spans_width() {
        // First point at x=0, last point at x=width, all in between
        // equally spaced.
        let values: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let out = points_attr(&values, 90.0, 20.0);
        let coords: Vec<&str> = out.split_whitespace().collect();
        assert_eq!(coords.len(), 10);
        let first_x: f32 = coords[0].split(',').next().unwrap().parse().unwrap();
        let last_x: f32 = coords[9].split(',').next().unwrap().parse().unwrap();
        assert!((first_x - 0.0).abs() < 0.01, "first x: {first_x}");
        assert!((last_x - 90.0).abs() < 0.01, "last x: {last_x}");
    }

    #[test]
    fn points_attr_respects_y_margin() {
        // The peak point should not sit at y=0 (the top edge) — the margin
        // pushes it inward.
        let out = points_attr(&[0.0, 10.0], 80.0, 20.0);
        let coords: Vec<&str> = out.split_whitespace().collect();
        // index 1 (value 10.0) is the max → smallest y. With 15% margin on
        // a 20px box, the top y should be 3.0, not 0.0.
        let y_top: f32 = coords[1].split(',').nth(1).unwrap().parse().unwrap();
        assert!(
            (y_top - 3.0).abs() < 0.01,
            "y_top should be 3.0 (15% margin), got: {y_top}",
        );
    }

    #[test]
    fn last_point_extracts_final_coord() {
        let points = "0.00,15.00 40.00,10.00 80.00,5.00";
        let (x, y) = last_point(points).unwrap();
        assert!((x - 80.0).abs() < 0.01);
        assert!((y - 5.0).abs() < 0.01);
    }

    #[test]
    fn last_point_empty() {
        assert_eq!(last_point(""), None);
    }
}
