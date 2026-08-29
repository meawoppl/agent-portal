/**
 * rizzma-mount — mount a portable figure (`.riz`) into a canvas.
 *
 * The host supplies the renderer. That is not a convenience parameter, it is
 * the security model: a digest an artifact carries about itself proves nothing,
 * because a hostile artifact can inline a hostile renderer and honestly report
 * its hash. So there is no default that reaches for a URL, no fallback to bytes
 * an artifact brought with it, and no way to ask this module to "just figure it
 * out". Vet a runtime once, keep your own copy, pass it in.
 *
 * Pairs with `rizzma::portable::inspect`, which reads an artifact's size,
 * poster, and schema without instantiating any of this.
 *
 * ## Verify this file before you run it
 *
 * This module is itself an executable supply-chain input, and it is the part
 * that decides whether the renderer gets verified at all — so verifying the
 * wasm with an unverified loader verifies nothing. A host must check every
 * executable asset against its own pinned digests *before* bootstrapping the
 * realm that runs them, which means the check cannot happen in here. The
 * published `runtime.json` carries a role, size, and sha256 for each of the
 * renderer, the glue, and this loader; pin that manifest and let it
 * authenticate the rest.
 *
 * Concretely: fetch bytes, verify the manifest against a digest your registry
 * pins, parse it, verify each asset against its entry, and only then create the
 * sandbox and hand it bytes you have already checked. If you materialise the
 * verified JS as blob URLs, revoke them on dispose.
 *
 * @module rizzma-mount
 */

const MAGIC = 0x47465a52; // "RZFG", little-endian
const TAG_JSON = "JSON";
const TAG_PSTR = "PSTR";

/**
 * Read the chunk directory of an artifact.
 *
 * @param {Uint8Array} bytes
 * @returns {{tag: string, offset: number, length: number}[]}
 */
export function chunks(bytes) {
  if (bytes.length < 12) throw new Error("riz: shorter than the 12-byte header");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== MAGIC) throw new Error("riz: bad magic");
  const declared = view.getUint32(8, true);
  if (declared !== bytes.length) {
    throw new Error(`riz: declared length ${declared} but got ${bytes.length}`);
  }
  const out = [];
  let pos = 12;
  const decoder = new TextDecoder("ascii");
  while (pos < bytes.length) {
    if (pos + 8 > bytes.length) throw new Error("riz: truncated chunk header");
    const length = view.getUint32(pos, true);
    const tag = decoder.decode(bytes.subarray(pos + 4, pos + 8));
    if (pos + 8 + length > bytes.length) throw new Error("riz: truncated chunk payload");
    out.push({ tag, offset: pos + 8, length });
    pos += 8 + length;
    while (pos % 8 !== 0) pos += 1; // padding
  }
  return out;
}

/**
 * Read an artifact's metadata without instantiating a renderer.
 *
 * Use this to size a card, announce alt text, and decide between mounting and
 * showing the poster — before spending a renderer download on it.
 *
 * @param {Uint8Array} bytes
 * @returns {{schema: number, generator: object, renderer: object, meta: object|null,
 *            timeline: {duration: number, loop: boolean}|null,
 *            poster: Uint8Array|null}}
 */
export function readMetadata(bytes) {
  const dir = chunks(bytes);
  const json = dir.find((c) => c.tag === TAG_JSON);
  if (!json) throw new Error("riz: missing JSON chunk");
  const spec = JSON.parse(
    new TextDecoder().decode(bytes.subarray(json.offset, json.offset + json.length)),
  );
  const pstr = dir.find((c) => c.tag === TAG_PSTR);
  return {
    schema: spec.schema,
    generator: spec.generator,
    renderer: spec.renderer,
    meta: spec.meta ?? null,
    timeline: spec.timeline
      ? { duration: spec.timeline.duration, loop: spec.timeline.loop }
      : null,
    poster: pstr ? bytes.subarray(pstr.offset, pstr.offset + pstr.length) : null,
  };
}

/** A blob URL for an artifact's poster, or null when it carries none. */
export function posterUrl(bytes) {
  const { poster } = readMetadata(bytes);
  return poster ? URL.createObjectURL(new Blob([poster], { type: "image/png" })) : null;
}

/**
 * Instantiated renderers in *this realm*, keyed by digest.
 *
 * Two constraints meet here, and they are the same one seen from opposite
 * sides. A module cache cannot cross an opaque-origin boundary, so a host that
 * sandboxes each figure in its own iframe compiles once per frame no matter
 * what this map does — the answer to that cost is keeping few frames live, not
 * sharing a realm between artifacts. And because this map outlives any
 * individual `dispose()`, it is exactly the state that would leak from one
 * artifact to the next if a realm *were* reused. So don't: disposal is
 * terminal, the realm goes with it, and this map dies with the realm. Pooling,
 * if it is ever worth the complexity, needs an explicit reset that proves
 * every prior resource is gone — not a second `mount()` after a `dispose()`.
 */
const modules = new Map(); // digest -> Promise<wasm module namespace>

/** Lowercase hex SHA-256 of `bytes`, via SubtleCrypto. */
async function digest(bytes) {
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(hash))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Mount `bytes` into `canvas` using a renderer the host chose.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {Uint8Array} bytes the artifact
 * @param {object} options
 * @param {{wasm: ArrayBuffer|Uint8Array, glue: string, sha256?: string}} options.renderer
 *   Required. `wasm` is the renderer you vetted, `glue` is the URL of the
 *   matching wasm-bindgen JS, and `sha256` — when given — is the digest you
 *   expect, checked before instantiation. Note the digest that matters is the
 *   one *you* recorded when you vetted these bytes, not the one the artifact
 *   claims.
 * @param {number} [options.maxBytes] artifact budget, default 10 MiB.
 * @param {boolean} [options.autoplay] start an animated figure playing. The
 *   host owns this decision and must pause on viewport exit, document hidden,
 *   and session-view unfocus — nothing else will (design/11 §7).
 * @returns {Promise<{figure: object, session: object, animated: boolean,
 *   duration: number, play: () => void, pause: () => void,
 *   seek: (t: number) => void, dispose: () => void}>}
 */
export async function mount(canvas, bytes, options) {
  const renderer = options?.renderer;
  if (!renderer?.wasm || !renderer?.glue) {
    throw new Error(
      "riz: mount() requires options.renderer = {wasm, glue}. " +
        "The host chooses the renderer; an artifact's own digest never authorizes one.",
    );
  }
  const wasmBytes =
    renderer.wasm instanceof Uint8Array ? renderer.wasm : new Uint8Array(renderer.wasm);

  const actual = await digest(wasmBytes);
  if (renderer.sha256 && renderer.sha256.toLowerCase() !== actual) {
    throw new Error(
      `riz: renderer digest mismatch — expected ${renderer.sha256}, got ${actual}`,
    );
  }

  // One instantiation per distinct renderer *within this realm*. A module cache
  // cannot cross an opaque-origin iframe boundary, so a sandbox-per-figure host
  // pays a compile per frame; keeping few frames live is the answer, not
  // relaxing isolation to share this map.
  let modulePromise = modules.get(actual);
  if (!modulePromise) {
    modulePromise = (async () => {
      const mod = await import(renderer.glue);
      await mod.default({ module_or_path: wasmBytes });
      return mod;
    })();
    modules.set(actual, modulePromise);
  }
  const mod = await modulePromise;

  const [min, max] = mod.WasmFigure.schemaRange();
  const { schema, meta } = readMetadata(bytes);
  if (schema < min || schema > max) {
    throw new Error(
      `riz: artifact is schema ${schema}; this renderer draws ${min}..=${max} — show the poster instead`,
    );
  }
  if (meta && canvas.width === 0 && canvas.height === 0) {
    canvas.width = meta.width_px;
    canvas.height = meta.height_px;
  }
  if (meta?.alt && !canvas.getAttribute("aria-label")) {
    canvas.setAttribute("role", "img");
    canvas.setAttribute("aria-label", meta.alt);
  }

  const figure = mod.WasmFigure.from_portable(bytes, options?.maxBytes ?? 0);
  const session = figure.bind(canvas.id);

  // Playback: the mount handle owns the requestAnimationFrame clock while
  // playing (a message per frame would put the host's event loop in the frame
  // path — design/11 §7); the host owns *whether* it plays, and must pause on
  // viewport exit, document hidden, and session-view unfocus. Seeks ride the
  // session's own rAF-coalesced repaint, so scrubbing faster than the display
  // refreshes does not stack frames.
  const [animated, duration] = session.animation();
  const tl = readMetadata(bytes).timeline;
  const looping = tl ? !!tl.loop : false;
  let playing = false;
  let t = 0;
  let lastTick = 0;
  let raf = 0;

  const tick = (now) => {
    if (!playing) return;
    t += (now - lastTick) / 1000;
    lastTick = now;
    if (!looping && t >= duration) {
      // Natural completion: clamp, paint the end, stop the loop.
      t = duration;
      playing = false;
      session.seek(t);
      return;
    }
    session.seek(t); // the session wraps/clamps; we keep a monotonic clock
    raf = requestAnimationFrame(tick);
  };

  const handle = {
    figure,
    session,
    /** Whether this figure animates, and its duration in seconds. */
    animated: animated > 0,
    duration,
    /** Start (or restart, at the end of a non-looping run) playback. */
    play() {
      if (!(animated > 0) || playing) return;
      if (!looping && t >= duration) t = 0; // resume-at-end restarts
      playing = true;
      lastTick = performance.now();
      raf = requestAnimationFrame(tick);
    },
    /** Stop the clock, keeping the current frame. Frees nothing. */
    pause() {
      playing = false;
      cancelAnimationFrame(raf);
    },
    /** Jump to `time` seconds and repaint; legal playing or paused. */
    seek(time) {
      t = time;
      lastTick = performance.now();
      session.seek(t);
    },
    /** Cancel the clock, free the wasm session, release the backing store. */
    dispose() {
      playing = false;
      cancelAnimationFrame(raf);
      session.free();
      canvas.width = 0;
      canvas.height = 0;
    },
  };
  if (options?.autoplay && animated > 0) handle.play();
  return handle;
}
