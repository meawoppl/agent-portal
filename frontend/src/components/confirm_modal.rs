use crate::hooks::{use_escape_capture, use_focus_trap};
use yew::prelude::*;

/// Visual style of the confirm modal. Each variant maps to an existing CSS
/// class family so extracted call sites render pixel-identically.
#[derive(Clone, Copy, PartialEq, Default)]
pub enum ConfirmModalStyle {
    /// `modal-content confirm-modal` container with `modal-*` buttons (admin page).
    #[default]
    Standard,
    /// Bare `confirm-modal` container with `cancel-button`/`confirm-button` (settings panels).
    Panel,
    /// `modal-content delete-confirm` container with `modal-*` buttons (dashboard delete/leave).
    Danger,
}

impl ConfirmModalStyle {
    fn container_class(&self) -> &'static str {
        match self {
            Self::Standard => "modal-content confirm-modal",
            Self::Panel => "confirm-modal",
            Self::Danger => "modal-content delete-confirm",
        }
    }

    fn actions_class(&self) -> &'static str {
        match self {
            Self::Panel => "confirm-actions",
            Self::Standard | Self::Danger => "modal-actions",
        }
    }

    fn cancel_class(&self) -> &'static str {
        match self {
            Self::Panel => "cancel-button",
            Self::Standard | Self::Danger => "modal-cancel",
        }
    }

    fn confirm_class(&self) -> &'static str {
        match self {
            Self::Panel => "confirm-button",
            Self::Standard | Self::Danger => "modal-confirm",
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct ConfirmModalProps {
    /// Main message body of the modal.
    pub message: AttrValue,
    /// Optional heading rendered above the message.
    #[prop_or_default]
    pub title: Option<AttrValue>,
    /// Optional warning line rendered below the message.
    #[prop_or_default]
    pub warning: Option<AttrValue>,
    /// Label for the confirm button.
    #[prop_or(AttrValue::Static("Confirm"))]
    pub confirm_label: AttrValue,
    #[prop_or_default]
    pub style: ConfirmModalStyle,
    pub on_confirm: Callback<MouseEvent>,
    pub on_cancel: Callback<MouseEvent>,
}

#[function_component(ConfirmModal)]
pub fn confirm_modal(props: &ConfirmModalProps) -> Html {
    // Keyboard access (#1384): trap Tab within the modal and focus its first
    // control (Cancel, rendered first — the safer default for a destructive
    // action) on open. Enter/Space activate the focused button natively.
    let container = use_node_ref();
    use_focus_trap(container.clone());

    // Escape cancels. Capture-phase so it never reaches the bubble-phase
    // session-interrupt / nav-mode handlers underneath (mirrors the help
    // overlay). `on_cancel` takes a `MouseEvent`, so synthesize a click for the
    // keyboard path; the cancel callbacks ignore the event's contents.
    {
        let on_cancel = props.on_cancel.clone();
        use_escape_capture(
            true,
            Callback::from(move |()| {
                if let Ok(ev) = web_sys::MouseEvent::new("click") {
                    on_cancel.emit(ev);
                }
            }),
        );
    }

    html! {
        <div class="modal-overlay" onclick={props.on_cancel.clone()}>
            <div
                ref={container}
                class={props.style.container_class()}
                onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
            >
                if let Some(title) = &props.title {
                    <h2>{ title }</h2>
                }
                <p>{ &props.message }</p>
                if let Some(warning) = &props.warning {
                    <p class="modal-warning">{ warning }</p>
                }
                <div class={props.style.actions_class()}>
                    <button type="button" class={props.style.cancel_class()} onclick={props.on_cancel.clone()}>
                        { "Cancel" }
                    </button>
                    <button type="button" class={props.style.confirm_class()} onclick={props.on_confirm.clone()}>
                        { &props.confirm_label }
                    </button>
                </div>
            </div>
        </div>
    }
}
