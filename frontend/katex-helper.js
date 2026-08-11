// Helper exposed on window for Yew to call for a single math region.
//
// Yew calls this once per `MathSpan` element with that region's LaTeX source.
// KaTeX renders INTO the supplied element and owns everything inside it; Yew
// renders that element with no children of its own, so the two never touch the
// same nodes.
//
// This deliberately does NOT use KaTeX's `auto-render` extension. That
// extension scans a subtree and rewrites matching text nodes in place — when
// pointed at Yew-rendered markdown it replaced nodes Yew's bundle still
// referenced, so the next re-render (every streamed token) computed an insert
// position against a detached node and panicked the WASM app with
// "failed to insert node before next sibling". It also applied its own
// delimiter heuristics, which disagreed with the Rust-side scanner that
// already skips code spans and dollar amounts.
//
// KaTeX loads from a deferred <script>, so on a cold page load Yew can call
// this before the library exists. Such calls leave the LaTeX source visible as
// text and are queued, then flushed when KaTeX becomes available.

(function () {
    function options(displayMode) {
        return {
            displayMode: !!displayMode,
            throwOnError: false,
            errorColor: '#cc6666',
            // `trust` stays false (the default): \href/\url and friends are
            // refused, so agent-authored math cannot inject navigable links.
            trust: false,
        };
    }

    // element -> {latex, displayMode} for calls that arrived before KaTeX did.
    const pending = new Map();
    let pollHandle = null;

    function ready() {
        return typeof window.katex === 'object'
            && window.katex !== null
            && typeof window.katex.render === 'function';
    }

    function renderNow(element, latex, displayMode) {
        try {
            window.katex.render(latex, element, options(displayMode));
            return true;
        } catch (e) {
            console.error('[katex] render failed:', e);
            // Leave the source readable rather than an empty gap.
            element.textContent = latex;
            return false;
        }
    }

    function flush() {
        if (!ready()) return;
        for (const [element, spec] of pending) {
            // Elements Yew has since removed are skipped, not resurrected.
            if (element.isConnected) {
                renderNow(element, spec.latex, spec.displayMode);
            }
        }
        pending.clear();
        if (pollHandle) {
            clearInterval(pollHandle);
            pollHandle = null;
        }
    }

    function schedulePoll() {
        if (pollHandle) return;
        pollHandle = setInterval(flush, 50);
    }

    window.renderMathIntoNode = function (element, latex, displayMode) {
        if (!element) {
            console.warn('[katex] renderMathIntoNode called with no element');
            return;
        }
        if (ready()) {
            pending.delete(element);
            renderNow(element, latex, displayMode);
            return;
        }
        element.textContent = latex;
        pending.set(element, { latex: latex, displayMode: displayMode });
        schedulePoll();
    };
})();
