const RUNTIMES = new Map([
  ["1.9.0", {
    root: "/rizzma-runtime/1.9.0",
    manifestSha256: "abf71fea0fa053893f5c530aa7b9401fa212b4d77f6788950a5cf8209e141aa4",
  }],
  ["1.10.0", {
    root: "/rizzma-runtime/1.10.0",
    manifestSha256: "30c38615e990a3994c90f1a99ea4dcc42d8595a404ec8e0ea89303a86e930c48",
  }],
]);
const MAX_ARTIFACT_BYTES = 10 * 1024 * 1024;
const active = [];

async function sha256(bytes) {
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function verifiedRuntime(version) {
  const runtime = RUNTIMES.get(version);
  if (!runtime) throw new Error("portable-figure runtime is not registered");
  const manifestResponse = await fetch(`${runtime.root}/runtime.json`);
  if (!manifestResponse.ok) throw new Error("portable-figure runtime manifest is unavailable");
  const manifestBytes = await manifestResponse.arrayBuffer();
  if (await sha256(manifestBytes) !== runtime.manifestSha256) {
    throw new Error("portable-figure runtime manifest failed verification");
  }
  const manifest = JSON.parse(new TextDecoder().decode(manifestBytes));
  if (manifest.manifest !== 1 || manifest.version !== version
      || manifest.schema_min !== 1 || manifest.schema_max !== 3) {
    throw new Error("unsupported portable-figure runtime manifest");
  }
  const expected = new Map([
    ["renderer", ["rizzma_bg.wasm", "application/wasm"]],
    ["glue", ["rizzma.js", "text/javascript"]],
    ["loader", ["rizzma-mount.js", "text/javascript"]],
  ]);
  if (!Array.isArray(manifest.assets) || manifest.assets.length !== expected.size) {
    throw new Error("portable-figure runtime manifest has the wrong asset set");
  }
  const result = {};
  for (const asset of manifest.assets) {
    const rule = expected.get(asset.role);
    if (!rule || result[asset.role] || asset.file !== rule[0] || asset.mime !== rule[1]
        || !Number.isSafeInteger(asset.size) || asset.size <= 0
        || !/^[a-f0-9]{64}$/.test(asset.sha256)) {
      throw new Error("portable-figure runtime manifest is invalid");
    }
    const response = await fetch(`${runtime.root}/${asset.file}`);
    if (!response.ok) throw new Error(`portable-figure runtime asset ${asset.role} is unavailable`);
    const bytes = await response.arrayBuffer();
    if (bytes.byteLength !== asset.size || await sha256(bytes) !== asset.sha256) {
      throw new Error(`portable-figure runtime asset ${asset.role} failed verification`);
    }
    result[asset.role] = bytes;
    result[`${asset.role}Sha256`] = asset.sha256;
  }
  return result;
}

function childDocument(nonce) {
  const script = `
    let terminal = false;
    let mounted = null;
    let urls = [];
    let duration = 0;
    let looping = false;
    let playing = false;
    let position = 0;
    let clockStartedAt = 0;
    let progressTimer = 0;
    const boot = async (event) => {
      if (terminal || event.source !== parent || event.data?.kind !== "rizzma-bootstrap"
          || event.data?.nonce !== ${JSON.stringify(nonce)} || event.ports.length !== 1) return;
      terminal = true;
      removeEventListener("message", boot);
      const port = event.ports[0];
      try {
        const loaderUrl = URL.createObjectURL(new Blob([event.data.loader], {type:"text/javascript"}));
        urls = [loaderUrl];
        const loader = await import(loaderUrl);
        let lastSeq = 0;
        let state = "ready";
        let disposeSeq = null;
        // Rizzma owns the frame clock. Mirror only its playhead at 4 Hz so the
        // host controls stay responsive without putting cross-realm messages
        // on every animation frame.
        const emitPlaybackState = (re) => {
          const reply = {nonce:${JSON.stringify(nonce)}, type:"state", playing, time:position};
          if (re !== undefined) reply.re = re;
          port.postMessage(reply);
        };
        const updatePosition = () => {
          if (!playing) return;
          const now = performance.now();
          position += (now - clockStartedAt) / 1000;
          clockStartedAt = now;
          if (looping && duration > 0) {
            position %= duration;
          } else if (position >= duration) {
            position = duration;
            playing = false;
            clearInterval(progressTimer);
            progressTimer = 0;
          }
        };
        const startProgress = () => {
          clearInterval(progressTimer);
          progressTimer = setInterval(() => {
            updatePosition();
            emitPlaybackState();
          }, 250);
        };
        const cleanup = () => {
          clearInterval(progressTimer);
          progressTimer = 0;
          playing = false;
          try { mounted?.dispose(); } catch (_) {}
          const canvas = document.getElementById("figure");
          canvas.width = 0; canvas.height = 0;
          for (const url of urls) URL.revokeObjectURL(url);
          urls = [];
          state = "disposed";
        };
        port.onmessage = async (message) => {
          const request = message.data;
          if (request?.nonce !== ${JSON.stringify(nonce)} || !Number.isSafeInteger(request.seq)
              || request.seq <= lastSeq) return;
          lastSeq = request.seq;
          if (request.type === "mount" && state === "ready") {
            try {
              state = "mounting";
              const glueUrl = URL.createObjectURL(new Blob([request.glue], {type:"text/javascript"}));
              urls.push(glueUrl);
              const canvas = document.getElementById("figure");
              const artifactBytes = new Uint8Array(request.artifact);
              const metadata = loader.readMetadata(artifactBytes);
              looping = Boolean(metadata.timeline?.loop);
              mounted = await loader.mount(canvas, artifactBytes, {
                renderer: {wasm: request.wasm, glue: glueUrl, sha256: request.wasmSha256},
                maxBytes: ${MAX_ARTIFACT_BYTES},
                autoplay: false
              });
              if (disposeSeq !== null) {
                cleanup();
                port.postMessage({nonce:${JSON.stringify(nonce)}, re:request.seq, type:"superseded", by:disposeSeq});
                port.postMessage({nonce:${JSON.stringify(nonce)}, re:disposeSeq, type:"disposed"});
                port.close();
              } else {
                state = "mounted";
                duration = Number(mounted.duration) || 0;
                port.postMessage({nonce:${JSON.stringify(nonce)}, re:request.seq, type:"mounted",
                  animated:Boolean(mounted.animated), duration});
                emitPlaybackState();
              }
            } catch (error) {
              state = "failed";
              port.postMessage({nonce:${JSON.stringify(nonce)}, re:request.seq, type:"error", message:String(error).slice(0,256)});
            }
          } else if (request.type === "play" && state === "mounted") {
            if (!looping && position >= duration) position = 0;
            mounted.play();
            playing = Boolean(mounted.animated);
            clockStartedAt = performance.now();
            if (playing) startProgress();
            emitPlaybackState(request.seq);
          } else if (request.type === "pause" && state === "mounted") {
            updatePosition();
            mounted.pause();
            playing = false;
            clearInterval(progressTimer);
            progressTimer = 0;
            emitPlaybackState(request.seq);
          } else if (request.type === "seek" && state === "mounted"
              && Number.isFinite(request.time)) {
            position = Math.max(0, Math.min(duration, request.time));
            mounted.seek(position);
            clockStartedAt = performance.now();
            emitPlaybackState(request.seq);
          } else if (request.type === "dispose") {
            if (state === "mounting") { disposeSeq = request.seq; return; }
            cleanup();
            port.postMessage({nonce:${JSON.stringify(nonce)}, re:request.seq, type:"disposed"});
            port.close();
          } else {
            port.postMessage({nonce:${JSON.stringify(nonce)}, re:request.seq, type:"error", message:"illegal portable-figure protocol transition"});
          }
        };
        port.postMessage({nonce:${JSON.stringify(nonce)}, type:"ready", protocol:1});
      } catch (error) {
        port.postMessage({nonce:${JSON.stringify(nonce)}, type:"error", message:String(error).slice(0,256)});
      }
    };
    addEventListener("message", boot);`;
  return `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; connect-src 'none'; img-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}' blob: 'wasm-unsafe-eval'">
    <style>html,body{width:100%;height:100%;margin:0;background:#16161e;color:#c0caf5}body{display:flex;align-items:center;justify-content:center;overflow:hidden}canvas{display:block;max-width:100%;max-height:100%;width:auto;height:auto}</style>
    <canvas id="figure"></canvas><script nonce="${nonce}">${script}<\/script>`;
}

function disposeEntry(entry) {
  if (entry.disposed) return;
  entry.disposed = true;
  entry.port?.postMessage({nonce:entry.nonce, seq:entry.nextSeq++, type:"dispose"});
  setTimeout(() => entry.iframe.removeAttribute("srcdoc"), 500);
  const index = active.indexOf(entry);
  if (index >= 0) active.splice(index, 1);
}

export async function mountRizzma(iframe, artifactUrl, rendererVersion) {
  if (!(iframe instanceof HTMLIFrameElement)) throw new Error("portable-figure frame is missing");
  while (active.length >= 2) disposeEntry(active[0]);
  const artifactResponse = await fetch(artifactUrl, {credentials:"same-origin"});
  if (!artifactResponse.ok) throw new Error("portable figure expired or is unavailable");
  const artifact = await artifactResponse.arrayBuffer();
  if (artifact.byteLength === 0 || artifact.byteLength > MAX_ARTIFACT_BYTES) {
    throw new Error("portable figure exceeds the renderer budget");
  }
  const magic = new Uint8Array(artifact, 0, Math.min(4, artifact.byteLength));
  if (magic.length !== 4 || String.fromCharCode(...magic) !== "RZFG") {
    throw new Error("portable figure has invalid framing");
  }
  const runtime = await verifiedRuntime(rendererVersion);
  const nonceBytes = crypto.getRandomValues(new Uint8Array(16));
  const nonce = Array.from(nonceBytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  const channel = new MessageChannel();
  const entry = {iframe, nonce, port:channel.port1, disposed:false, nextSeq:1};
  active.push(entry);
  const loaded = new Promise((resolve) => iframe.addEventListener("load", resolve, {once:true}));
  iframe.srcdoc = childDocument(nonce);
  await loaded;
  const loader = new TextDecoder().decode(runtime.loader);
  const glue = new TextDecoder().decode(runtime.glue);
  return await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { disposeEntry(entry); reject(new Error("portable figure mount timed out")); }, 30000);
    channel.port1.onmessage = (event) => {
      if (event.data?.nonce !== nonce) return;
      if (event.data.type === "ready") {
        const seq = entry.nextSeq++;
        channel.port1.postMessage({nonce, seq, type:"mount", glue, wasm:runtime.renderer,
          artifact, wasmSha256:runtime.rendererSha256},
          [runtime.renderer, artifact]);
      }
      if (event.data.type === "mounted") { clearTimeout(timeout); resolve(event.data); }
      if (event.data.type === "state") {
        iframe.dataset.rizzmaPlaying = event.data.playing ? "true" : "false";
        iframe.dataset.rizzmaTime = String(Number(event.data.time) || 0);
        iframe.dispatchEvent(new Event("rizzma-state"));
      }
      if (event.data.type === "error") { clearTimeout(timeout); disposeEntry(entry); reject(new Error(event.data.message)); }
    };
    iframe.contentWindow.postMessage({kind:"rizzma-bootstrap", nonce, loader}, "*", [channel.port2]);
  });
}

function sendControl(iframe, type, fields = {}) {
  const entry = active.find((candidate) => candidate.iframe === iframe && !candidate.disposed);
  if (!entry) return;
  entry.port.postMessage({nonce:entry.nonce, seq:entry.nextSeq++, type, ...fields});
}

export function playRizzma(iframe) {
  sendControl(iframe, "play");
}

export function pauseRizzma(iframe) {
  sendControl(iframe, "pause");
}

export function seekRizzma(iframe, time) {
  if (Number.isFinite(time)) sendControl(iframe, "seek", {time});
}

export function disposeRizzma(iframe) {
  const entry = active.find((candidate) => candidate.iframe === iframe);
  if (entry) disposeEntry(entry);
}
