// Helper exposed on window for Yew to call after rendering markdown.
// Uses KaTeX's auto-render extension to find $...$ and $$...$$ in a DOM
// element and replace them with rendered math.
window.renderMathInNode = function(element) {
    if (!element || typeof window.renderMathInElement !== 'function') {
        return;
    }
    try {
        window.renderMathInElement(element, {
            delimiters: [
                { left: '$$', right: '$$', display: true },
                { left: '$', right: '$', display: false },
                { left: '\\(', right: '\\)', display: false },
                { left: '\\[', right: '\\]', display: true },
            ],
            // Don't try to render inside these elements (code blocks, inline code, etc.)
            ignoredTags: ['script', 'noscript', 'style', 'textarea', 'pre', 'code'],
            ignoredClasses: ['md-code-block', 'md-inline-code', 'tool-result-content', 'bash-command-inline'],
            // Don't crash on bad math — just leave the source visible
            throwOnError: false,
            errorColor: '#cc6666',
        });
    } catch (e) {
        // Auto-render not loaded yet or other failure — silently skip
    }
};
