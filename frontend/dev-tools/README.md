# dev-tools

Standalone harnesses for debugging frontend behavior in isolation. Not built
or shipped — open the HTML file directly in a browser.

## katex-isolated.html

Loads the production KaTeX scripts/version from the same CDN and renders a
hard-coded sample message through `renderMathInElement` with the exact
options (delimiters, ignored classes, error color) the app uses. Edit the
textarea and click **Render** to test arbitrary content.

Use this to disentangle "KaTeX can't render this math" from "our Yew/WASM
pipeline isn't calling KaTeX correctly". If KaTeX renders the math here
but not in the deployed app, the bug is on our side. If it fails to render
here too, the bug is in the math itself (unsupported command, malformed
delimiter, etc.).

To open:

```sh
xdg-open frontend/dev-tools/katex-isolated.html
# or just open it from your file manager
```

No server required.
