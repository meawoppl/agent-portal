//! Safe pointer corridor from the last point in the trigger to the submenu's
//! near edge. This follows the actual panel height, including a scrolled list,
//! and supports a panel placed on either side without swallowing clicks.
#[derive(Clone, Copy)]
pub(super) struct Rect {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

impl From<web_sys::DomRect> for Rect {
    fn from(rect: web_sys::DomRect) -> Self {
        Self {
            left: rect.left(),
            right: rect.right(),
            top: rect.top(),
            bottom: rect.bottom(),
        }
    }
}

impl Rect {
    pub(super) fn contains(self, (x, y): (f64, f64)) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }
}

pub(super) fn toward_panel(origin: (f64, f64), point: (f64, f64), panel: Rect) -> bool {
    let edge = if origin.0 < panel.left {
        panel.left
    } else if origin.0 > panel.right {
        panel.right
    } else {
        // Touch layouts put the panel below the trigger; no sideways corridor.
        return false;
    };
    let progress = (point.0 - origin.0) / (edge - origin.0);
    if !(0.0..=1.0).contains(&progress) {
        return false;
    }
    // A small tolerance covers the border/gap without a large hover blanket.
    let top = origin.1 + (panel.top - 8.0 - origin.1) * progress;
    let bottom = origin.1 + (panel.bottom + 8.0 - origin.1) * progress;
    point.1 >= top && point.1 <= bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagonal_paths_to_both_ends_cross_the_gap() {
        let panel = Rect {
            left: 204.0,
            right: 424.0,
            top: 80.0,
            bottom: 300.0,
        };
        for p in [(150.0, 105.0), (150.0, 210.0), (202.0, 290.0)] {
            assert!(toward_panel((100.0, 130.0), p, panel));
        }
        assert!(!toward_panel((100.0, 130.0), (90.0, 130.0), panel));
        assert!(!toward_panel((100.0, 130.0), (120.0, 280.0), panel));
        assert!(!toward_panel((100.0, 130.0), (430.0, 130.0), panel));
    }
    #[test]
    fn left_panel_and_inline_touch_layout() {
        let panel = Rect {
            left: 0.0,
            right: 200.0,
            top: 80.0,
            bottom: 300.0,
        };
        assert!(toward_panel((300.0, 130.0), (250.0, 200.0), panel));
        assert!(!toward_panel((300.0, 130.0), (320.0, 130.0), panel));
        assert!(!toward_panel((100.0, 40.0), (100.0, 60.0), panel));
    }
}
