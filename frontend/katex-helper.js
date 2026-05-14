// Helper exposed on window for Yew to call after rendering markdown.
// Uses KaTeX's auto-render extension to find $...$ and $$...$$ in a DOM
// element and replace them with rendered math.
//
// The KaTeX library and its auto-render extension are loaded via deferred
// <script> tags in index.html. On a cold page load, Yew may invoke this
// helper before those scripts have finished evaluating. The original
// implementation silently returned in that case, leaving math un-rendered
// for the lifetime of the page (the Yew effect is keyed on props.text and
// won't fire again for a message that doesn't change). We queue calls
// until KaTeX is ready and flush them on first availability.

(function () {
    const KATEX_OPTIONS = {
        delimiters: [
            { left: '$$', right: '$$', display: true },
            { left: '$', right: '$', display: false },
            { left: '\\(', right: '\\)', display: false },
            { left: '\\[', right: '\\]', display: true },
        ],
        ignoredTags: ['script', 'noscript', 'style', 'textarea', 'pre', 'code'],
        ignoredClasses: ['md-code-block', 'md-inline-code', 'tool-result-content', 'bash-command-inline'],
        throwOnError: false,
        errorColor: '#cc6666',
    };

    const pending = new Set();
    let pollHandle = null;

    function flush() {
        if (typeof window.renderMathInElement !== 'function') return;
        for (const element of pending) {
            if (!element.isConnected) continue;
            try {
                window.renderMathInElement(element, KATEX_OPTIONS);
            } catch (e) {
                console.error('[katex] render failed:', e);
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
        pollHandle = setInterval(function () {
            if (typeof window.renderMathInElement === 'function') {
                flush();
            }
        }, 50);
    }

    window.renderMathInNode = function (element) {
        if (!element) return;
        if (typeof window.renderMathInElement === 'function') {
            try {
                window.renderMathInElement(element, KATEX_OPTIONS);
            } catch (e) {
                console.error('[katex] render failed:', e);
            }
        } else {
            pending.add(element);
            schedulePoll();
        }
    };
})();
