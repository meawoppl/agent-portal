//! Pointer, wheel, and pinch state for the image lightbox.
//!
//! Pure geometry with no DOM types, so the zoom and pan math is unit-tested
//! instead of eyeballed. The component owns one [`LightboxGesture`] in a
//! `use_mut_ref` and mirrors its view into `use_state` for rendering; the
//! gesture itself must not live in `use_state`, because two fingers moving in
//! the same frame would each read the stale pre-frame value and one finger's
//! update would overwrite the other's.
//!
//! All screen points are viewport (`clientX/clientY`) coordinates; `center` is
//! the viewport center the image is laid out around, so a point's offset from
//! `center` is what the CSS `translate`/`scale` transform is measured against.

pub(super) const MIN_SCALE: f64 = 0.5;
pub(super) const MAX_SCALE: f64 = 12.0;

/// Wheel zoom rate per pixel of `deltaY` for a plain mouse wheel; one 100px
/// notch is about 16% (`exp(0.17)`), close to the old fixed 18% step.
const WHEEL_RATE: f64 = 0.0017;
/// Rate for ctrl+wheel. Browsers deliver a trackpad pinch as small ctrl+wheel
/// deltas (a few pixels each), so it needs a much larger gain to feel direct.
const PINCH_WHEEL_RATE: f64 = 0.01;
/// One wheel event never zooms by more than a factor of `exp(0.7)` (about 2x),
/// at either rate, so a high-velocity flick or an unusually large delta can't
/// jump the image away. Bounding the exponent rather than the raw delta keeps
/// that true for the plain and the ctrl+wheel gains alike.
const MAX_WHEEL_EXPONENT: f64 = 0.7;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct LightboxView {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}

impl Default for LightboxView {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        }
    }
}

impl LightboxView {
    /// Clamp the zoom, and snap back to the untransformed view at or below 1x
    /// so zooming all the way out re-centers the image.
    fn constrain(self) -> Self {
        let scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        if scale <= 1.0 {
            return Self::default();
        }
        Self { scale, ..self }
    }

    /// The `style` attribute value for the lightbox `<img>`. Yew writes a
    /// `style` string verbatim as the whole attribute, so this must be a full
    /// `property: value` declaration; a bare transform list is an invalid
    /// declaration the browser silently drops, which leaves zoom and pan
    /// updating state but never reaching the pixels.
    pub fn style(self) -> String {
        format!(
            "transform: translate({:.1}px, {:.1}px) scale({:.4})",
            self.x, self.y, self.scale
        )
    }

    /// Zoom to `scale`, keeping the image point under `focus` (an offset from
    /// the viewport center) where it is on screen.
    fn zoomed_about(self, focus: Point, scale: f64) -> Self {
        let ratio = scale / self.scale.max(0.001);
        Self {
            scale,
            x: focus.x - (focus.x - self.x) * ratio,
            y: focus.y - (focus.y - self.y) * ratio,
        }
        .constrain()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn minus(self, other: Point) -> Point {
        Point::new(self.x - other.x, self.y - other.y)
    }

    fn midpoint(self, other: Point) -> Point {
        Point::new((self.x + other.x) / 2.0, (self.y + other.y) / 2.0)
    }

    fn distance(self, other: Point) -> f64 {
        let d = self.minus(other);
        d.x.hypot(d.y)
    }
}

/// What the current pointers are doing, captured when the pointer set last
/// changed. Moves are computed against this fixed baseline rather than
/// accumulated frame to frame, so a gesture never drifts.
#[derive(Clone, Copy, Debug)]
enum Baseline {
    Idle,
    Drag {
        start: Point,
        view: LightboxView,
    },
    Pinch {
        mid: Point,
        distance: f64,
        view: LightboxView,
    },
}

#[derive(Debug)]
pub(super) struct LightboxGesture {
    view: LightboxView,
    pointers: Vec<(i32, Point)>,
    baseline: Baseline,
}

impl Default for LightboxGesture {
    fn default() -> Self {
        Self {
            view: LightboxView::default(),
            pointers: Vec::new(),
            baseline: Baseline::Idle,
        }
    }
}

impl LightboxGesture {
    pub fn view(&self) -> LightboxView {
        self.view
    }

    /// Whether a finger or button is currently down (drives the grabbing cursor).
    pub fn is_active(&self) -> bool {
        !self.pointers.is_empty()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reset the zoom but keep tracking pointers that are still down, so the
    /// Reset button doesn't strand a gesture in progress.
    pub fn reset_view(&mut self) {
        self.view = LightboxView::default();
        self.rebaseline();
    }

    /// A pointer went down. Only two are tracked; further ones are ignored.
    pub fn pointer_down(&mut self, id: i32, at: Point) {
        if self.pointers.iter().any(|(existing, _)| *existing == id) || self.pointers.len() >= 2 {
            return;
        }
        self.pointers.push((id, at));
        self.rebaseline();
    }

    /// A tracked pointer moved. Returns whether it was tracked (and so whether
    /// the view may have changed).
    pub fn pointer_move(&mut self, id: i32, at: Point, center: Point) -> bool {
        let Some(slot) = self
            .pointers
            .iter_mut()
            .find(|(existing, _)| *existing == id)
        else {
            return false;
        };
        slot.1 = at;

        self.view = match (self.baseline, self.pointers.as_slice()) {
            (Baseline::Drag { start, view }, [(_, now)]) => LightboxView {
                x: view.x + now.x - start.x,
                y: view.y + now.y - start.y,
                ..view
            }
            .constrain(),
            (
                Baseline::Pinch {
                    mid,
                    distance,
                    view,
                },
                [(_, a), (_, b)],
            ) => {
                let scale = view.scale * a.distance(*b).max(1.0) / distance;
                // The image point that sat under the fingers' midpoint when the
                // pinch began must follow the midpoint now: that is both the
                // zoom focus and, as the midpoint travels, a two-finger pan.
                let anchor = mid.minus(center);
                let image_x = (anchor.x - view.x) / view.scale;
                let image_y = (anchor.y - view.y) / view.scale;
                let target = a.midpoint(*b).minus(center);
                let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
                LightboxView {
                    scale,
                    x: target.x - image_x * scale,
                    y: target.y - image_y * scale,
                }
                .constrain()
            }
            _ => self.view,
        };
        true
    }

    /// A pointer lifted or was cancelled. Lifting one finger of a pinch carries
    /// on as a drag from where the view is now, with no jump.
    pub fn pointer_up(&mut self, id: i32) {
        self.pointers.retain(|(existing, _)| *existing != id);
        self.rebaseline();
    }

    /// A wheel event at `at`. `delta` is `deltaY` normalized to pixels
    /// (negative zooms in); `ctrl` marks the ctrl+wheel a trackpad pinch emits.
    pub fn wheel(&mut self, at: Point, delta: f64, ctrl: bool, center: Point) {
        let rate = if ctrl { PINCH_WHEEL_RATE } else { WHEEL_RATE };
        let exponent = (-delta * rate).clamp(-MAX_WHEEL_EXPONENT, MAX_WHEEL_EXPONENT);
        let scale = (self.view.scale * exponent.exp()).clamp(MIN_SCALE, MAX_SCALE);
        self.view = self.view.zoomed_about(at.minus(center), scale);
        self.rebaseline();
    }

    fn rebaseline(&mut self) {
        self.baseline = match self.pointers.as_slice() {
            [] => Baseline::Idle,
            [(_, at)] => Baseline::Drag {
                start: *at,
                view: self.view,
            },
            [(_, a), (_, b)] => Baseline::Pinch {
                mid: a.midpoint(*b),
                distance: a.distance(*b).max(1.0),
                view: self.view,
            },
            _ => Baseline::Idle,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CENTER: Point = Point { x: 500.0, y: 400.0 };

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    /// Where a point offset `from_center` in the untransformed image lands on
    /// screen for `view`.
    fn screen_of(view: LightboxView, image_offset: Point) -> Point {
        p(
            CENTER.x + view.x + image_offset.x * view.scale,
            CENTER.y + view.y + image_offset.y * view.scale,
        )
    }

    #[test]
    fn style_is_a_complete_transform_declaration() {
        let view = LightboxView {
            scale: 2.0,
            x: 10.0,
            y: -4.5,
        };
        assert_eq!(
            view.style(),
            "transform: translate(10.0px, -4.5px) scale(2.0000)"
        );
        assert!(LightboxView::default().style().starts_with("transform: "));
    }

    #[test]
    fn wheel_zooms_in_and_out_and_returns_to_rest() {
        let mut g = LightboxGesture::default();
        g.wheel(CENTER, -100.0, false, CENTER);
        let zoomed = g.view().scale;
        assert!(zoomed > 1.1 && zoomed < 1.25, "one notch is ~16%: {zoomed}");
        g.wheel(CENTER, 100.0, false, CENTER);
        let rest = g.view();
        assert!((rest.scale - 1.0).abs() < 1e-9, "{rest:?}");
        assert!(rest.x.abs() < 1e-9 && rest.y.abs() < 1e-9, "{rest:?}");
    }

    #[test]
    fn wheel_keeps_the_point_under_the_pointer_fixed() {
        let mut g = LightboxGesture::default();
        let pointer = p(620.0, 330.0);
        // The image point under the pointer before zooming.
        let under = LightboxView::default();
        let image_offset = p(
            (pointer.x - CENTER.x - under.x) / under.scale,
            (pointer.y - CENTER.y - under.y) / under.scale,
        );
        for _ in 0..4 {
            g.wheel(pointer, -100.0, false, CENTER);
        }
        let on_screen = screen_of(g.view(), image_offset);
        assert!((on_screen.x - pointer.x).abs() < 1e-6);
        assert!((on_screen.y - pointer.y).abs() < 1e-6);
    }

    #[test]
    fn ctrl_wheel_pinch_deltas_zoom_proportionally_and_are_clamped() {
        let mut slow = LightboxGesture::default();
        slow.wheel(CENTER, -5.0, true, CENTER);
        let mut fast = LightboxGesture::default();
        fast.wheel(CENTER, -20.0, true, CENTER);
        assert!(fast.view().scale > slow.view().scale);

        // One event is bounded to ~2x whichever gain it arrives with.
        let bound = MAX_WHEEL_EXPONENT.exp();
        for ctrl in [false, true] {
            let mut flick = LightboxGesture::default();
            flick.wheel(CENTER, -1_000_000.0, ctrl, CENTER);
            assert!((flick.view().scale - bound).abs() < 1e-9, "ctrl={ctrl}");

            let mut out = LightboxGesture::default();
            out.wheel(CENTER, -1_000_000.0, ctrl, CENTER);
            out.wheel(CENTER, 1_000_000.0, ctrl, CENTER);
            assert_eq!(out.view(), LightboxView::default(), "ctrl={ctrl}");
        }
    }

    #[test]
    fn zoom_is_clamped_to_the_maximum() {
        let mut g = LightboxGesture::default();
        for _ in 0..200 {
            g.wheel(CENTER, -400.0, false, CENTER);
        }
        assert_eq!(g.view().scale, MAX_SCALE);
    }

    #[test]
    fn single_pointer_drags_when_zoomed_and_snaps_home_at_rest() {
        let mut g = LightboxGesture::default();
        g.wheel(CENTER, -400.0, false, CENTER);
        let zoomed = g.view();
        g.pointer_down(1, p(100.0, 100.0));
        assert!(g.pointer_move(1, p(130.0, 90.0), CENTER));
        assert_eq!(g.view().x, zoomed.x + 30.0);
        assert_eq!(g.view().y, zoomed.y - 10.0);
        assert_eq!(g.view().scale, zoomed.scale);

        // At 1x there is nothing to pan: constrain re-centers the image.
        let mut rest = LightboxGesture::default();
        rest.pointer_down(1, p(100.0, 100.0));
        rest.pointer_move(1, p(180.0, 150.0), CENTER);
        assert_eq!(rest.view(), LightboxView::default());
    }

    #[test]
    fn moving_an_untracked_pointer_changes_nothing() {
        let mut g = LightboxGesture::default();
        assert!(!g.pointer_move(9, p(1.0, 1.0), CENTER));
        assert_eq!(g.view(), LightboxView::default());
    }

    #[test]
    fn pinch_scales_by_the_change_in_finger_distance() {
        let mut g = LightboxGesture::default();
        g.pointer_down(1, p(450.0, 400.0));
        g.pointer_down(2, p(550.0, 400.0));
        // Spread the fingers to double their distance about the same midpoint.
        g.pointer_move(1, p(400.0, 400.0), CENTER);
        g.pointer_move(2, p(600.0, 400.0), CENTER);
        assert!((g.view().scale - 2.0).abs() < 1e-9);
        // The midpoint is the viewport center here, so no translation.
        assert!(g.view().x.abs() < 1e-9 && g.view().y.abs() < 1e-9);
    }

    #[test]
    fn pinch_keeps_the_image_point_under_the_midpoint() {
        let mut g = LightboxGesture::default();
        let mid = p(620.0, 330.0);
        g.pointer_down(1, p(mid.x - 40.0, mid.y));
        g.pointer_down(2, p(mid.x + 40.0, mid.y));
        let image_offset = p(mid.x - CENTER.x, mid.y - CENTER.y);

        g.pointer_move(1, p(mid.x - 80.0, mid.y), CENTER);
        g.pointer_move(2, p(mid.x + 80.0, mid.y), CENTER);
        assert!((g.view().scale - 2.0).abs() < 1e-9);
        let on_screen = screen_of(g.view(), image_offset);
        assert!((on_screen.x - mid.x).abs() < 1e-6);
        assert!((on_screen.y - mid.y).abs() < 1e-6);
    }

    #[test]
    fn pinch_midpoint_travel_pans_the_image() {
        let mut g = LightboxGesture::default();
        g.pointer_down(1, p(450.0, 400.0));
        g.pointer_down(2, p(550.0, 400.0));
        // Same finger distance, both fingers shifted: pure pan, scale 1 snaps
        // home, so zoom in first to make a pan observable.
        g.wheel(CENTER, -400.0, false, CENTER);
        let before = g.view();
        g.pointer_move(1, p(470.0, 380.0), CENTER);
        g.pointer_move(2, p(570.0, 380.0), CENTER);
        assert!((g.view().scale - before.scale).abs() < 1e-9);
        assert!((g.view().x - (before.x + 20.0)).abs() < 1e-9);
        assert!((g.view().y - (before.y - 20.0)).abs() < 1e-9);
    }

    #[test]
    fn lifting_one_finger_continues_as_a_drag_without_a_jump() {
        let mut g = LightboxGesture::default();
        g.wheel(CENTER, -400.0, false, CENTER);
        g.pointer_down(1, p(450.0, 400.0));
        g.pointer_down(2, p(550.0, 400.0));
        g.pointer_move(1, p(400.0, 400.0), CENTER);
        g.pointer_move(2, p(600.0, 400.0), CENTER);
        let pinched = g.view();

        g.pointer_up(2);
        assert!(g.is_active());
        g.pointer_move(1, p(400.0, 400.0), CENTER);
        assert_eq!(g.view(), pinched, "no jump when a finger lifts");
        g.pointer_move(1, p(410.0, 395.0), CENTER);
        assert_eq!(g.view().x, pinched.x + 10.0);
        assert_eq!(g.view().y, pinched.y - 5.0);

        g.pointer_up(1);
        assert!(!g.is_active());
    }

    #[test]
    fn a_third_pointer_is_ignored() {
        let mut g = LightboxGesture::default();
        g.pointer_down(1, p(0.0, 0.0));
        g.pointer_down(2, p(10.0, 0.0));
        g.pointer_down(3, p(20.0, 0.0));
        assert!(!g.pointer_move(3, p(30.0, 0.0), CENTER));
        g.pointer_up(3);
        assert!(g.is_active(), "ignored pointer doesn't end the gesture");
    }

    #[test]
    fn reset_view_keeps_fingers_tracked_and_reset_clears_everything() {
        let mut g = LightboxGesture::default();
        g.wheel(CENTER, -400.0, false, CENTER);
        g.pointer_down(1, p(0.0, 0.0));
        g.reset_view();
        assert_eq!(g.view(), LightboxView::default());
        assert!(g.is_active());

        g.reset();
        assert!(!g.is_active());
        assert_eq!(g.view(), LightboxView::default());
    }
}
