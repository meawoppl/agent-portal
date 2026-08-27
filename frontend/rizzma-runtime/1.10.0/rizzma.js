/* @ts-self-types="./rizzma.d.ts" */

/**
 * An [`Axes3D`](crate::mplot3d::Axes3D) owned across the wasm boundary.
 *
 * 3D scenes render as full frames rather than through the interactive
 * session machinery: build the scene once, then call `render` (wasm only)
 * each time the view changes — `set_view` plus a JS interval is a spinning
 * plot. `width_px`/`height_px`/`dpi` are fixed at construction and match
 * [`Axes3D::render_png`](crate::mplot3d::Axes3D::render_png) semantics
 * (`dpi` scales titles and decorations).
 */
export class WasmAxes3D {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmAxes3DFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmaxes3d_free(ptr, 0);
    }
    /**
     * Create an empty `width_px` by `height_px` scene rendered at `dpi`.
     * @param {number} width_px
     * @param {number} height_px
     * @param {number} dpi
     */
    constructor(width_px, height_px, dpi) {
        const ret = wasm.wasmaxes3d_new(width_px, height_px, dpi);
        this.__wbg_ptr = ret;
        WasmAxes3DFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Add a flat-shaded colormapped surface over the `x` × `y` grid; `z` is
     * row-major with `x.len() * y.len()` heights. Degenerate input adds
     * nothing.
     * @param {Float64Array} x
     * @param {Float64Array} y
     * @param {Float64Array} z
     */
    plot_surface(x, y, z) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayF64ToWasm0(z, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        wasm.wasmaxes3d_plot_surface(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
    }
    /**
     * Render the scene onto the canvas element with id `canvas_id`,
     * HiDPI-crisp: the backing store is `devicePixelRatio` × the logical
     * pixel size (decorations scale to match) and the canvas CSS size is
     * set to the logical size. Call again after `set_view` to animate.
     *
     * # Errors
     *
     * Returns a [`JsValue`] error if the canvas element cannot be found,
     * is not a canvas, has no 2D context, or `putImageData` fails.
     * @param {string} canvas_id
     */
    render(canvas_id) {
        const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmaxes3d_render(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Add a cloud of 3D scatter markers (common prefix of the slices).
     * @param {Float64Array} x
     * @param {Float64Array} y
     * @param {Float64Array} z
     */
    scatter3d(x, y, z) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayF64ToWasm0(z, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        wasm.wasmaxes3d_scatter3d(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
    }
    /**
     * Set a title drawn centered at the top of the canvas.
     * @param {string} title
     */
    set_title(title) {
        const ptr0 = passStringToWasm0(title, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmaxes3d_set_title(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Set the elevation and azimuth view angles in degrees.
     * @param {number} elev
     * @param {number} azim
     */
    set_view(elev, azim) {
        wasm.wasmaxes3d_set_view(this.__wbg_ptr, elev, azim);
    }
}
if (Symbol.dispose) WasmAxes3D.prototype[Symbol.dispose] = WasmAxes3D.prototype.free;

/**
 * A [`Figure`] owned across the wasm boundary, with an interactive
 * pixel-to-data readout for DOM hover.
 *
 * Construct one with [`WasmFigure::sample`], read its pixel size via
 * [`WasmFigure::size`], render it to a canvas with `WasmFigure::render`
 * (wasm only — `#[cfg(target_arch = "wasm32")]`, so it can't be an intra-doc
 * link on the host docs build), and translate cursor pixels to data
 * coordinates with [`WasmFigure::data_at`].
 */
export class WasmFigure {
    static __wrap(ptr) {
        const obj = Object.create(WasmFigure.prototype);
        obj.__wbg_ptr = ptr;
        WasmFigureFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmFigureFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmfigure_free(ptr, 0);
    }
    /**
     * Add axes at the figure-fraction rectangle `(left, bottom, width,
     * height)`, returning the new axes' index.
     * @param {number} l
     * @param {number} b
     * @param {number} w
     * @param {number} h
     * @returns {number}
     */
    add_axes(l, b, w, h) {
        const ret = wasm.wasmfigure_add_axes(this.__wbg_ptr, l, b, w, h);
        return ret >>> 0;
    }
    /**
     * Add axes for 1-based cell `index` of an `nrows` x `ncols` grid,
     * returning the new axes' index.
     *
     * # Errors
     *
     * Returns an error if `index` is zero or exceeds `nrows * ncols`.
     * @param {number} nrows
     * @param {number} ncols
     * @param {number} index
     * @returns {number}
     */
    add_subplot(nrows, ncols, index) {
        const ret = wasm.wasmfigure_add_subplot(this.__wbg_ptr, nrows, ncols, index);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Whether this figure animates, and for how long in seconds.
     *
     * Returned as `[animated, duration]` so a loader can decide whether to
     * show transport controls. `animated` is `1.0` or `0.0`.
     * @returns {Float64Array}
     */
    animation() {
        const ret = wasm.wasmfigure_animation(this.__wbg_ptr);
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
    /**
     * Consume this figure and attach it to the canvas with id `canvas_id`
     * as an interactive session: HiDPI rendering plus wheel zoom
     * (anchored at the cursor), left-drag pan, double-click reset, and a
     * hover callback. Keep the returned [`WasmSession`] alive for as long
     * as the canvas should stay interactive.
     *
     * # Errors
     *
     * Returns a [`JsValue`] error if the canvas element cannot be found,
     * is not a canvas, has no 2D context, or rendering fails.
     * @param {string} canvas_id
     * @returns {WasmSession}
     */
    bind(canvas_id) {
        const ptr = this.__destroy_into_raw();
        const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_bind(ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WasmSession.__wrap(ret[0]);
    }
    /**
     * Map a **top-down canvas pixel** `(px, py)` to data coordinates in the
     * figure's first axes.
     *
     * Returns `Some([x, y])` when the pixel falls inside the axes rectangle,
     * else `None`. Across the wasm boundary this maps to a
     * `Float64Array | undefined`, so a hover readout can show `undefined`
     * (off-axes) versus a concrete `[x, y]`.
     * @param {number} px
     * @param {number} py
     * @returns {Float64Array | undefined}
     */
    data_at(px, py) {
        const ret = wasm.wasmfigure_data_at(this.__wbg_ptr, px, py);
        let v1;
        if (ret[0] !== 0) {
            v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        }
        return v1;
    }
    /**
     * Rebuild a figure from portable-figure (`.riz`) bytes.
     *
     * This is the browser end of the artifact: the host fetches the bytes,
     * hands them over, and the result binds to a canvas like any other
     * [`WasmFigure`], with pan, zoom, and hover intact — because the artifact
     * carries the data, not a picture of it.
     *
     * `max_bytes` is the caller's budget for the whole artifact. Artifacts are
     * attacker-influenced input, and the host is the one that knows what is
     * reasonable, so there is no default: pass the same cap used when the
     * artifact was accepted. `0` means "the library default" (10 MiB).
     *
     * # Errors
     *
     * Returns the error message as a `JsValue` when the artifact is
     * malformed, over budget, or of a schema this build cannot render. Callers
     * should fall back to the poster rather than showing an empty frame; see
     * `rizzma-portable`'s `inspect`, which reports both without instantiating
     * this module.
     * @param {Uint8Array} bytes
     * @param {number} max_bytes
     * @returns {WasmFigure}
     */
    static from_portable(bytes, max_bytes) {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_from_portable(ptr0, len0, max_bytes);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WasmFigure.__wrap(ret[0]);
    }
    /**
     * Display row-major scalar `data` (`nrows` × `ncols`) as a colormapped
     * image on axes `axes` — `extent` is `[x0, x1, y0, y1]` in data space,
     * `cmap` a colormap name (empty string for the default), and
     * `vmin`/`vmax` the fixed normalization bounds (live updates through
     * `WasmSession::set_image_data` keep them, so streaming frames don't
     * flicker). Data row `0` sits at the top of the extent.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range, `extent` is not 4 numbers,
     * or `data.len()` is not `nrows * ncols`.
     * @param {number} axes
     * @param {Float64Array} data
     * @param {number} nrows
     * @param {number} ncols
     * @param {Float64Array} extent
     * @param {string} cmap
     * @param {number} vmin
     * @param {number} vmax
     */
    imshow(axes, data, nrows, ncols, extent, cmap, vmin, vmax) {
        const ptr0 = passArrayF64ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(extent, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(cmap, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_imshow(this.__wbg_ptr, axes, ptr0, len0, nrows, ncols, ptr1, len1, ptr2, len2, vmin, vmax);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Add a legend to axes `axes`: label `i` is paired with the color of the
     * `i`-th plotted line.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range or there are more labels
     * than lines.
     * @param {number} axes
     * @param {string[]} labels
     */
    legend(axes, labels) {
        const ptr0 = passArrayJsValueToWasm0(labels, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_legend(this.__wbg_ptr, axes, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The effective `[xlo, xhi, ylo, yhi]` limits of axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @returns {Float64Array}
     */
    limits(axes) {
        const ret = wasm.wasmfigure_limits(this.__wbg_ptr, axes);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
    /**
     * Create an empty `width_in` by `height_in` inch figure (default DPI).
     * @param {number} width_in
     * @param {number} height_in
     */
    constructor(width_in, height_in) {
        const ret = wasm.wasmfigure_new(width_in, height_in);
        this.__wbg_ptr = ret;
        WasmFigureFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Switch axes `axes` to oscilloscope styling: CRT background, fixed
     * phosphor graticule, phosphor trace cycle, and in-frame corner
     * readouts — built to stay legible at any size, down to sparkline
     * strips. Call before plotting so traces pick up the phosphor cycle.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     */
    oscilloscope(axes) {
        const ret = wasm.wasmfigure_oscilloscope(this.__wbg_ptr, axes);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Plot `y` against `x` as a line on axes `axes`, using the color cycle.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {Float64Array} x
     * @param {Float64Array} y
     */
    plot(axes, x, y) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_plot(this.__wbg_ptr, axes, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Plot a styled line: `style` is a plain object with optional keys
     * `color` (matplotlib color spec string), `lw` (points), and `ls`
     * (`'-'`, `'--'`, `':'`, `'-.'` or long names). Unknown keys are errors.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range or `style` is invalid.
     * @param {number} axes
     * @param {Float64Array} x
     * @param {Float64Array} y
     * @param {any} style
     */
    plot_styled(axes, x, y, style) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_plot_styled(this.__wbg_ptr, axes, ptr0, len0, ptr1, len1, style);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Render this figure onto the canvas element with id `canvas_id`,
     * HiDPI-crisp: the backing store is `devicePixelRatio` × the figure's
     * logical pixel size and the canvas CSS size is set to the logical
     * size.
     *
     * # Errors
     *
     * Returns a [`JsValue`] error if the canvas element cannot be found, is
     * not a canvas, has no 2D context, or `ImageData`/`putImageData` fails.
     * @param {string} canvas_id
     */
    render(canvas_id) {
        const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_render(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Build a [`WasmFigure`] wrapping the built-in [`sample_figure`].
     * @returns {WasmFigure}
     */
    static sample() {
        const ret = wasm.wasmfigure_sample();
        return WasmFigure.__wrap(ret);
    }
    /**
     * Scatter-plot `y` against `x` on axes `axes`, using the color cycle.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {Float64Array} x
     * @param {Float64Array} y
     */
    scatter(axes, x, y) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_scatter(this.__wbg_ptr, axes, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The schema range this build can render, as `[min, max]`.
     *
     * A loader compares this against an artifact's declared schema to decide
     * between mounting it and showing its poster.
     * @returns {Uint32Array}
     */
    static schemaRange() {
        const ret = wasm.wasmfigure_schemaRange();
        var v1 = getArrayU32FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 4, 4);
        return v1;
    }
    /**
     * Move this figure to time `t` (seconds), applying its animation.
     *
     * Wrapped or clamped into the timeline first, so a caller can hand over a
     * monotonically increasing clock. A static figure ignores this.
     *
     * # Errors
     *
     * Returns the error message when a track addresses an artist that does not
     * exist or hands it the wrong number of values.
     * @param {number} t
     */
    seek(t) {
        const ret = wasm.wasmfigure_seek(this.__wbg_ptr, t);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set the figure's canvas background color from a matplotlib-style
     * color spec (name, hex, `tab:*`, `C0`…) — e.g. a dark face behind
     * full-bleed oscilloscope strips.
     *
     * # Errors
     *
     * Returns an error if the color spec is not recognized.
     * @param {string} color
     */
    set_facecolor(color) {
        const ptr0 = passStringToWasm0(color, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_set_facecolor(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Replace the data of line `line` on axes `axes` in place (live
     * updates), keeping its style. Autoscaled limits re-derive; explicit
     * limits are untouched.
     *
     * # Errors
     *
     * Returns an error if `axes` or `line` is out of range.
     * @param {number} axes
     * @param {number} line
     * @param {Float64Array} x
     * @param {Float64Array} y
     */
    set_line_data(axes, line, x, y) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_set_line_data(this.__wbg_ptr, axes, line, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set the title of axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {string} title
     */
    set_title(axes, title) {
        const ptr0 = passStringToWasm0(title, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_set_title(this.__wbg_ptr, axes, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set the x-axis label of axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {string} label
     */
    set_xlabel(axes, label) {
        const ptr0 = passStringToWasm0(label, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_set_xlabel(this.__wbg_ptr, axes, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set explicit x limits on axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {number} lo
     * @param {number} hi
     */
    set_xlim(axes, lo, hi) {
        const ret = wasm.wasmfigure_set_xlim(this.__wbg_ptr, axes, lo, hi);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Switch axes `axes` to a log-scaled x axis with `base`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {number} base
     */
    set_xscale_log(axes, base) {
        const ret = wasm.wasmfigure_set_xscale_log(this.__wbg_ptr, axes, base);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set the y-axis label of axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {string} label
     */
    set_ylabel(axes, label) {
        const ptr0 = passStringToWasm0(label, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmfigure_set_ylabel(this.__wbg_ptr, axes, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Set explicit y limits on axes `axes`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {number} lo
     * @param {number} hi
     */
    set_ylim(axes, lo, hi) {
        const ret = wasm.wasmfigure_set_ylim(this.__wbg_ptr, axes, lo, hi);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Switch axes `axes` to a log-scaled y axis with `base`.
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @param {number} base
     */
    set_yscale_log(axes, base) {
        const ret = wasm.wasmfigure_set_yscale_log(this.__wbg_ptr, axes, base);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Link `follower`'s x-limits to `leader`'s (matplotlib's `sharex`):
     * pan/zoom on either axes keeps the pair's x in lockstep while each y
     * stays independent.
     *
     * # Errors
     *
     * Returns an error if either index is out of range, the two are equal,
     * or `leader` itself already follows another axes.
     * @param {number} follower
     * @param {number} leader
     */
    sharex(follower, leader) {
        const ret = wasm.wasmfigure_sharex(this.__wbg_ptr, follower, leader);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The figure's pixel size as a 2-element `[width, height]` array.
     *
     * Exposed across the wasm boundary as a `Float64Array`; callers size the
     * target canvas to `size[0]` by `size[1]`.
     * @returns {Float64Array}
     */
    get size() {
        const ret = wasm.wasmfigure_size(this.__wbg_ptr);
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
}
if (Symbol.dispose) WasmFigure.prototype[Symbol.dispose] = WasmFigure.prototype.free;

/**
 * An interactive figure bound to a canvas.
 *
 * Created by [`WasmFigure::bind`]. **Keep the session alive** (a
 * variable, an array, a field — any live JS reference) for as long as
 * the canvas should stay interactive. Dropping it — explicitly via
 * `session.free()` or implicitly when the GC finalizes an unreferenced
 * session — leaves the last frame on the canvas and detaches every DOM
 * listener it registered, so the canvas goes cleanly inert instead of
 * throwing "closure invoked after being dropped" on later events.
 */
export class WasmSession {
    static __wrap(ptr) {
        const obj = Object.create(WasmSession.prototype);
        obj.__wbg_ptr = ptr;
        WasmSessionFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmSessionFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmsession_free(ptr, 0);
    }
    /**
     * Whether the bound figure animates, and for how long, as
     * `[animated, duration]` (`animated` is `1.0` or `0.0`).
     * @returns {Float64Array}
     */
    animation() {
        const ret = wasm.wasmsession_animation(this.__wbg_ptr);
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
    /**
     * Map a **logical canvas pixel** to `[axes, x, y]` data coordinates,
     * or `undefined` when the pixel is over no axes.
     * @param {number} px
     * @param {number} py
     * @returns {Float64Array | undefined}
     */
    data_at(px, py) {
        const ret = wasm.wasmsession_data_at(this.__wbg_ptr, px, py);
        let v1;
        if (ret[0] !== 0) {
            v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        }
        return v1;
    }
    /**
     * The effective `[xlo, xhi, ylo, yhi]` limits of axes `axes` (the
     * live values pan/zoom mutate).
     *
     * # Errors
     *
     * Returns an error if `axes` is out of range.
     * @param {number} axes
     * @returns {Float64Array}
     */
    limits(axes) {
        const ret = wasm.wasmsession_limits(this.__wbg_ptr, axes);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
    /**
     * Register a hover callback, called as `cb(axes, x, y)` while the
     * cursor is over axes data and `cb(null)` when it leaves the canvas.
     * @param {Function} cb
     */
    on_hover(cb) {
        wasm.wasmsession_on_hover(this.__wbg_ptr, cb);
    }
    /**
     * Repaint the canvas now (outside the rAF coalescing).
     *
     * # Errors
     *
     * Returns a [`JsValue`] error if `ImageData`/`putImageData` fails.
     */
    render() {
        const ret = wasm.wasmsession_render(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Advance the bound figure's animation to time `t` (seconds) and
     * schedule a repaint.
     *
     * This is how a host animates a canvas it has already bound: the
     * session stays live, pointer and wheel listeners stay attached, and
     * a view the user has taken over by panning or zooming is preserved —
     * the timeline's `xlim`/`ylim` tracks yield to it until double-click
     * hands the view back (see `Interactor::seek`). Data tracks always
     * apply.
     *
     * Repaints are coalesced through the same `requestAnimationFrame`
     * path as interaction, so calling this faster than the display
     * refreshes does not stack frames. A figure without a timeline
     * ignores the call.
     *
     * # Errors
     *
     * Returns an error when a track addresses an artist that does not
     * exist or hands it the wrong number of values.
     * @param {number} t
     */
    seek(t) {
        const ret = wasm.wasmsession_seek(this.__wbg_ptr, t);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Replace the data and extent of image `image` on axes `axes` in
     * place (live updates — e.g. a scrolling spectrogram), keeping its
     * colormap and `vmin`/`vmax` normalization, and schedule a
     * rAF-coalesced repaint. `extent` is `[x0, x1, y0, y1]` in data
     * space.
     *
     * Autoscaled limits re-derive from the new extent; explicit limits
     * — including a view the user has panned/zoomed — are untouched.
     *
     * # Errors
     *
     * Returns an error if `axes` or `image` is out of range, `extent`
     * is not 4 numbers, or `data.len()` is not `nrows * ncols`.
     * @param {number} axes
     * @param {number} image
     * @param {Float64Array} data
     * @param {number} nrows
     * @param {number} ncols
     * @param {Float64Array} extent
     */
    set_image_data(axes, image, data, nrows, ncols, extent) {
        const ptr0 = passArrayF64ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(extent, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmsession_set_image_data(this.__wbg_ptr, axes, image, ptr0, len0, nrows, ncols, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Replace the data of line `line` on axes `axes` in place (live
     * updates) and schedule a rAF-coalesced repaint: a burst of updates
     * between frames paints once.
     *
     * Autoscaled limits re-derive from the new data; explicit limits —
     * including a view the user has panned/zoomed — are untouched.
     *
     * # Errors
     *
     * Returns an error if `axes` or `line` is out of range.
     * @param {number} axes
     * @param {number} line
     * @param {Float64Array} x
     * @param {Float64Array} y
     */
    set_line_data(axes, line, x, y) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmsession_set_line_data(this.__wbg_ptr, axes, line, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Replace the offsets of scatter collection `collection` on axes
     * `axes` in place (live updates), keeping its markers and styling,
     * and schedule a rAF-coalesced repaint. Only the common prefix of
     * `x` and `y` is used.
     *
     * Autoscaled limits re-derive from the new offsets; explicit limits
     * — including a view the user has panned/zoomed — are untouched.
     *
     * # Errors
     *
     * Returns an error if `axes` or `collection` is out of range.
     * @param {number} axes
     * @param {number} collection
     * @param {Float64Array} x
     * @param {Float64Array} y
     */
    set_scatter_offsets(axes, collection, x, y) {
        const ptr0 = passArrayF64ToWasm0(x, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF64ToWasm0(y, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmsession_set_scatter_offsets(this.__wbg_ptr, axes, collection, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The figure's **logical** pixel size as `[width, height]` (CSS
     * pixels; multiply by `devicePixelRatio` for the backing size).
     * @returns {Float64Array}
     */
    get size() {
        const ret = wasm.wasmsession_size(this.__wbg_ptr);
        var v1 = getArrayF64FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 8, 8);
        return v1;
    }
    /**
     * Record the cursor's data-space position into line `line` of axes
     * `axes` as a rolling trail of up to `capacity` points — updated
     * entirely in Rust as pointer events arrive, with no JS in the loop.
     *
     * Each repaint is rAF-coalesced like any other update, and pan/zoom
     * keep working while the trail records (the trail pauses during a
     * drag, when the cursor is panning rather than hovering).
     *
     * # Errors
     *
     * Returns an error if `axes` or `line` is out of range, or
     * `capacity` is zero.
     * @param {number} axes
     * @param {number} line
     * @param {number} capacity
     */
    track_cursor(axes, line, capacity) {
        const ret = wasm.wasmsession_track_cursor(this.__wbg_ptr, axes, line, capacity);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
}
if (Symbol.dispose) WasmSession.prototype[Symbol.dispose] = WasmSession.prototype.free;

/**
 * Render the built-in [`sample_figure`] onto the canvas element with id
 * `canvas_id` (HiDPI-crisp, non-interactive).
 *
 * # Errors
 *
 * Returns a [`JsValue`] error if the canvas element cannot be found, is not
 * a canvas, has no 2D context, or `ImageData`/`putImageData` fails.
 * @param {string} canvas_id
 */
export function draw_sample_to_canvas(canvas_id) {
    const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.draw_sample_to_canvas(ptr0, len0);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

/**
 * Blit a straight-RGBA8 buffer onto the canvas element with id
 * `canvas_id`, sizing the backing store to `width` by `height` device
 * pixels (no CSS sizing — presentation scale is the caller's concern).
 *
 * # Errors
 *
 * Returns a [`JsValue`] error if the canvas cannot be found or resolved to a
 * 2D context, or if constructing/placing the `ImageData` fails.
 * @param {string} canvas_id
 * @param {Uint8Array} rgba
 * @param {number} width
 * @param {number} height
 */
export function render_rgba_to_canvas(canvas_id, rgba, width, height) {
    const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(rgba, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.render_rgba_to_canvas(ptr0, len0, ptr1, len1, width, height);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_is_null_ea9085d691f535d3: function(arg0) {
            const ret = arg0 === null;
            return ret;
        },
        __wbg___wbindgen_is_undefined_c05833b95a3cf397: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_394265ed1e1b84ee: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_b0ca35b86a603356: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_344f42d3211c4765: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_fffb441def202758: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_addEventListener_520e749bbae24529: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3, arg4);
        }, arguments); },
        __wbg_addEventListener_d85450ee1320c989: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_button_f6a9a7b725f1838e: function(arg0) {
            const ret = arg0.button;
            return ret;
        },
        __wbg_call_44b7209e1e252e6a: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            const ret = arg0.call(arg1, arg2, arg3, arg4);
            return ret;
        }, arguments); },
        __wbg_call_a6e5c5dce5018821: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_currentTarget_a8ed7ef60b89f253: function(arg0) {
            const ret = arg0.currentTarget;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_deltaMode_d869228efd74f393: function(arg0) {
            const ret = arg0.deltaMode;
            return ret;
        },
        __wbg_deltaY_6cfce8f8da250c23: function(arg0) {
            const ret = arg0.deltaY;
            return ret;
        },
        __wbg_devicePixelRatio_1c0e0ed7deb19cd8: function(arg0) {
            const ret = arg0.devicePixelRatio;
            return ret;
        },
        __wbg_document_179650d6cb13c263: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_getBoundingClientRect_e828e6c31c66dea6: function(arg0) {
            const ret = arg0.getBoundingClientRect();
            return ret;
        },
        __wbg_getContext_e79ddf6a9cb3cc76: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getContext(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_getElementById_1cbd8f06dbe8eb8e: function(arg0, arg1, arg2) {
            const ret = arg0.getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_get_78f252d074a84d0b: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_unchecked_6e0ad6d2a41b06f6: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_height_6eec812c213259a1: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_height_96c07d9559d0200a: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_instanceof_CanvasRenderingContext2d_2284b703b7023dcc: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CanvasRenderingContext2D;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlCanvasElement_ed02ed9136056019: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLCanvasElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MouseEvent_89eddfc6203c1749: function(arg0) {
            let result;
            try {
                result = arg0 instanceof MouseEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Object_33f20e6f12439f3e: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Object;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_PointerEvent_8ef1feb51407c0ed: function(arg0) {
            let result;
            try {
                result = arg0 instanceof PointerEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_WheelEvent_8a8f43ee9318fcd4: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WheelEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_05ba1ee4f6781663: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_keys_58421f8f96795607: function(arg0) {
            const ret = Object.keys(arg0);
            return ret;
        },
        __wbg_length_370319915dc99107: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_new_da52cf8fe3429cb2: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_with_u8_clamped_array_and_sh_2767e4741c267d25: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new ImageData(getClampedArrayU8FromWasm0(arg0, arg1), arg2 >>> 0, arg3 >>> 0);
            return ret;
        }, arguments); },
        __wbg_offsetX_fdc5eb20edabaadb: function(arg0) {
            const ret = arg0.offsetX;
            return ret;
        },
        __wbg_offsetY_0a05e99022d21c5b: function(arg0) {
            const ret = arg0.offsetY;
            return ret;
        },
        __wbg_pointerId_ea33d2695be12e7f: function(arg0) {
            const ret = arg0.pointerId;
            return ret;
        },
        __wbg_preventDefault_b64888c857500682: function(arg0) {
            arg0.preventDefault();
        },
        __wbg_putImageData_a4dee11e08ab9ac8: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.putImageData(arg1, arg2, arg3);
        }, arguments); },
        __wbg_releasePointerCapture_3e982a4a25bf65a8: function() { return handleError(function (arg0, arg1) {
            arg0.releasePointerCapture(arg1);
        }, arguments); },
        __wbg_removeEventListener_a3f23c70077bdcc1: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_requestAnimationFrame_1a85deeab66448c2: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.requestAnimationFrame(arg1);
            return ret;
        }, arguments); },
        __wbg_setPointerCapture_70025ca3fb7f26b9: function() { return handleError(function (arg0, arg1) {
            arg0.setPointerCapture(arg1);
        }, arguments); },
        __wbg_setProperty_e4e51b1b1d681d15: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setProperty(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_set_height_7d9d8f892e6964c6: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_passive_86a651d25740d760: function(arg0, arg1) {
            arg0.passive = arg1 !== 0;
        },
        __wbg_set_width_8e30d010cd66830d: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_static_accessor_GLOBAL_4ef717fb391d88b7: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_8d1badc68b5a74f4: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_146583524fe1469b: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_f2829a2234d7819e: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_style_6657aed849e5d757: function(arg0) {
            const ret = arg0.style;
            return ret;
        },
        __wbg_width_219185400361db86: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbg_width_6d9315ecc7140ff6: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 267, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen__convert__closures_____invoke__hbc002ff235cde092);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 269, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen__convert__closures_____invoke__hed1c03189287acc5);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./rizzma_bg.js": import0,
    };
}

function wasm_bindgen__convert__closures_____invoke__hed1c03189287acc5(arg0, arg1) {
    wasm.wasm_bindgen__convert__closures_____invoke__hed1c03189287acc5(arg0, arg1);
}

function wasm_bindgen__convert__closures_____invoke__hbc002ff235cde092(arg0, arg1, arg2) {
    wasm.wasm_bindgen__convert__closures_____invoke__hbc002ff235cde092(arg0, arg1, arg2);
}

const WasmAxes3DFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmaxes3d_free(ptr, 1));
const WasmFigureFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmfigure_free(ptr, 1));
const WasmSessionFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmsession_free(ptr, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_destroy_closure(state.a, state.b));

function getArrayF64FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat64ArrayMemory0().subarray(ptr / 8, ptr / 8 + len);
}

function getArrayU32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getClampedArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ClampedArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

let cachedFloat64ArrayMemory0 = null;
function getFloat64ArrayMemory0() {
    if (cachedFloat64ArrayMemory0 === null || cachedFloat64ArrayMemory0.byteLength === 0) {
        cachedFloat64ArrayMemory0 = new Float64Array(wasm.memory.buffer);
    }
    return cachedFloat64ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint32ArrayMemory0 = null;
function getUint32ArrayMemory0() {
    if (cachedUint32ArrayMemory0 === null || cachedUint32ArrayMemory0.byteLength === 0) {
        cachedUint32ArrayMemory0 = new Uint32Array(wasm.memory.buffer);
    }
    return cachedUint32ArrayMemory0;
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

let cachedUint8ClampedArrayMemory0 = null;
function getUint8ClampedArrayMemory0() {
    if (cachedUint8ClampedArrayMemory0 === null || cachedUint8ClampedArrayMemory0.byteLength === 0) {
        cachedUint8ClampedArrayMemory0 = new Uint8ClampedArray(wasm.memory.buffer);
    }
    return cachedUint8ClampedArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_destroy_closure(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayF64ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 8, 8) >>> 0;
    getFloat64ArrayMemory0().set(arg, ptr / 8);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    for (let i = 0; i < array.length; i++) {
        const add = addToExternrefTable0(array[i]);
        getDataViewMemory0().setUint32(ptr + 4 * i, add, true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedFloat64ArrayMemory0 = null;
    cachedUint32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    cachedUint8ClampedArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('rizzma_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
