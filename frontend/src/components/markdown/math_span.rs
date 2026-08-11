use wasm_bindgen::JsCast;
use yew::prelude::*;

/// A single typeset math region — a leaf whose interior belongs to KaTeX.
///
/// **Why this is a childless element.** KaTeX renders by mutating the DOM. The
/// previous design pointed KaTeX's `auto-render` extension at the whole
/// rendered-markdown subtree, where it rewrote matching text nodes into
/// `<span class="katex">` trees *in place*. Those nodes were Yew's: its bundle
/// still referenced the text nodes KaTeX had replaced, so the next re-render —
/// and streamed messages re-render on every token — computed an insert
/// position against a node that was no longer a child of its parent.
/// `insertBefore` then threw `NotFoundError`, which Yew turns into a panic
/// (`failed to insert node before next sibling`) that aborts the whole WASM
/// app, taking the dashboard down.
///
/// Rendering `<span>` with **no Yew children** fixes that at the root: Yew's
/// bundle for this element is a single childless node, so Yew owns the element
/// and KaTeX owns everything inside it, and neither one ever walks into the
/// other's territory. Removal still works because dropping the element takes
/// KaTeX's subtree with it — no orphaned nodes.
#[derive(Properties, PartialEq)]
pub(super) struct MathSpanProps {
    /// LaTeX source, delimiters already stripped.
    pub latex: String,
    /// Display (block) vs. inline math.
    pub display: bool,
}

#[function_component(MathSpan)]
pub(super) fn math_span(props: &MathSpanProps) -> Html {
    let node_ref = use_node_ref();

    {
        let node_ref = node_ref.clone();
        use_effect_with(
            (props.latex.clone(), props.display),
            move |(latex, display)| {
                if let Some(element) = node_ref.cast::<web_sys::Element>() {
                    render_math_into(&element, latex, *display);
                }
                || ()
            },
        );
    }

    let class = if props.display {
        "md-math md-math-display"
    } else {
        "md-math"
    };
    html! { <span ref={node_ref} class={class} /> }
}

/// Hand one math region to the JS helper, which typesets it into `element`
/// (queueing until KaTeX's deferred script has evaluated). Silently a no-op if
/// the helper isn't present — the helper leaves the LaTeX source as readable
/// text in that case rather than showing an empty gap.
fn render_math_into(element: &web_sys::Element, latex: &str, display: bool) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(func) = js_sys::Reflect::get(&window, &"renderMathIntoNode".into()) else {
        return;
    };
    let Ok(func) = func.dyn_into::<js_sys::Function>() else {
        return;
    };
    let _ = func.call3(
        &window,
        element,
        &latex.into(),
        &wasm_bindgen::JsValue::from_bool(display),
    );
}
