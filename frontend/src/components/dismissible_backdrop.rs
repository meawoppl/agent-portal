//! Pointer-safe backdrop dismissal shared by modal overlays.
//!
//! A bubbling `click` is not sufficient here: when a text-selection drag starts
//! inside a dialog and ends outside it, browsers may target the synthesized
//! click at the backdrop (the nearest common ancestor). Dismiss only a pointer
//! gesture that both starts and ends directly on the backdrop.

use yew::prelude::*;

#[derive(Default)]
struct BackdropPress {
    pointer_id: Option<i32>,
}

impl BackdropPress {
    fn down(&mut self, pointer_id: i32, direct: bool) {
        self.pointer_id = direct.then_some(pointer_id);
    }

    fn up(&mut self, pointer_id: i32, direct: bool) -> bool {
        self.pointer_id.take() == Some(pointer_id) && direct
    }

    fn cancel(&mut self, pointer_id: i32) {
        if self.pointer_id == Some(pointer_id) {
            self.pointer_id = None;
        }
    }
}

fn is_direct(event: &PointerEvent) -> bool {
    event.target().is_some() && event.target() == event.current_target()
}

#[derive(Properties, PartialEq)]
pub struct DismissibleBackdropProps {
    pub class: AttrValue,
    pub on_close: Callback<()>,
    #[prop_or_default]
    pub children: Html,
}

#[function_component(DismissibleBackdrop)]
pub fn dismissible_backdrop(props: &DismissibleBackdropProps) -> Html {
    let press = use_mut_ref(BackdropPress::default);

    let onpointerdown = {
        let press = press.clone();
        Callback::from(move |event: PointerEvent| {
            press
                .borrow_mut()
                .down(event.pointer_id(), is_direct(&event));
        })
    };
    let onpointerup = {
        let press = press.clone();
        let on_close = props.on_close.clone();
        Callback::from(move |event: PointerEvent| {
            if press.borrow_mut().up(event.pointer_id(), is_direct(&event)) {
                on_close.emit(());
            }
        })
    };
    let onpointercancel = {
        let press = press.clone();
        Callback::from(move |event: PointerEvent| {
            press.borrow_mut().cancel(event.pointer_id());
        })
    };

    html! {
        <div
            class={props.class.clone()}
            {onpointerdown}
            {onpointerup}
            {onpointercancel}
        >
            {props.children.clone()}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::BackdropPress;

    #[test]
    fn closes_only_when_press_starts_and_ends_on_backdrop() {
        let mut press = BackdropPress::default();
        press.down(7, true);
        assert!(press.up(7, true));

        press.down(8, false);
        assert!(!press.up(8, true), "selection drag must not close modal");

        press.down(9, true);
        assert!(!press.up(9, false), "drag into pane must not close modal");
    }

    #[test]
    fn ignores_other_and_cancelled_pointers() {
        let mut press = BackdropPress::default();
        press.down(3, true);
        assert!(!press.up(4, true));

        press.down(5, true);
        press.cancel(5);
        assert!(!press.up(5, true));
    }
}
