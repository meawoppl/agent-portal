//! Optional foreground chime. Audio is unlocked by an explicit user gesture;
//! background delivery remains the operating system's notification sound.
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
let context;
const key = 'portal-ready-sound';
export function readySoundEnabled() {
    try { return localStorage.getItem(key) === 'true'; } catch (_) { return false; }
}
function unlock() {
    if (!readySoundEnabled()) return;
    try {
        context ||= new (window.AudioContext || window.webkitAudioContext)();
        context.resume().catch(() => {});
    } catch (_) {}
}
if (typeof document !== 'undefined') document.addEventListener('pointerdown', unlock, { passive: true });
export function readySoundSet(enabled) {
    try { localStorage.setItem(key, String(enabled)); } catch (_) {}
    if (enabled) unlock();
}
export function readySoundPlay() {
    if (!readySoundEnabled() || !context || context.state !== 'running') return;
    const start = context.currentTime;
    const tone = context.createOscillator();
    const gain = context.createGain();
    tone.frequency.setValueAtTime(660, start);
    tone.frequency.setValueAtTime(880, start + 0.09);
    gain.gain.setValueAtTime(0.035, start);
    gain.gain.exponentialRampToValueAtTime(0.001, start + 0.22);
    tone.connect(gain); gain.connect(context.destination);
    tone.start(start); tone.stop(start + 0.23);
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = readySoundEnabled)]
    pub fn enabled() -> bool;
    #[wasm_bindgen(js_name = readySoundSet)]
    pub fn set_enabled(enabled: bool);
    #[wasm_bindgen(js_name = readySoundPlay)]
    pub fn play();
}
