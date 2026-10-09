//! Opt-in audio with a single narration channel. Cancelling on navigation and
//! unmount prevents several chamber narrators speaking over one another.
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
let narration;
let context;
export function apertureStop() {
    if (narration) { narration.pause(); narration = undefined; }
}
export function apertureSpeak(id) {
    apertureStop();
    narration = new Audio('/aperture-assets/voice/' + id + '.mp3');
    narration.volume = 0.8;
    narration.play().catch(() => {});
}
export function apertureTone() {
    try {
        context ||= new (window.AudioContext || window.webkitAudioContext)();
        context.resume().catch(() => {});
        const start = context.currentTime;
        const oscillator = context.createOscillator();
        const gain = context.createGain();
        oscillator.type = 'sine';
        oscillator.frequency.setValueAtTime(160, start);
        oscillator.frequency.exponentialRampToValueAtTime(720, start + 0.22);
        oscillator.frequency.exponentialRampToValueAtTime(260, start + 0.55);
        gain.gain.setValueAtTime(0, start);
        gain.gain.linearRampToValueAtTime(0.09, start + 0.04);
        gain.gain.exponentialRampToValueAtTime(0.001, start + 0.6);
        oscillator.connect(gain); gain.connect(context.destination);
        oscillator.start(start); oscillator.stop(start + 0.65);
    } catch (_) { /* The exhibit remains fully usable without audio. */ }
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = apertureStop)]
    pub fn stop();
    #[wasm_bindgen(js_name = apertureSpeak)]
    pub fn speak(id: &str);
    #[wasm_bindgen(js_name = apertureTone)]
    pub fn tone();
}
