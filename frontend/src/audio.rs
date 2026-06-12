use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use web_sys::{AudioContext, AudioContextState, OscillatorType};

pub const STORAGE_KEY: &str = "claude-portal-sound-config";

const ACTIVITY_COOLDOWN_MS: f64 = 2000.0;
const DEFAULT_COOLDOWN_MS: f64 = 500.0;

thread_local! {
    static AUDIO_CTX: RefCell<Option<AudioContext>> = const { RefCell::new(None) };
    static LAST_PLAYED: RefCell<HashMap<SoundEvent, f64>> = RefCell::new(HashMap::new());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SoundEvent {
    Activity,
    Error,
    SessionSwap,
    AwaitingInput,
}

impl SoundEvent {
    pub fn label(&self) -> &'static str {
        match self {
            SoundEvent::Activity => "Activity",
            SoundEvent::Error => "Errors & Warnings",
            SoundEvent::SessionSwap => "Session Swap",
            SoundEvent::AwaitingInput => "Awaiting Input",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            SoundEvent::Activity => "New messages arriving in sessions",
            SoundEvent::Error => "API errors and rate limit warnings",
            SoundEvent::SessionSwap => "Switching between sessions",
            SoundEvent::AwaitingInput => "Session needs your input",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Waveform {
    Sine,
    Square,
    Sawtooth,
    Triangle,
}

impl Waveform {
    pub fn label(&self) -> &'static str {
        match self {
            Waveform::Sine => "Sine",
            Waveform::Square => "Square",
            Waveform::Sawtooth => "Sawtooth",
            Waveform::Triangle => "Triangle",
        }
    }

    pub fn all() -> &'static [Waveform] {
        &[
            Waveform::Sine,
            Waveform::Square,
            Waveform::Sawtooth,
            Waveform::Triangle,
        ]
    }

    fn to_oscillator_type(self) -> OscillatorType {
        match self {
            Waveform::Sine => OscillatorType::Sine,
            Waveform::Square => OscillatorType::Square,
            Waveform::Sawtooth => OscillatorType::Sawtooth,
            Waveform::Triangle => OscillatorType::Triangle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSound {
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
    pub frequency: f64,
    pub waveform: Waveform,
    pub volume: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundConfig {
    pub enabled: bool,
    pub activity: EventSound,
    pub error: EventSound,
    pub session_swap: EventSound,
    pub awaiting_input: EventSound,
}

impl SoundConfig {
    pub fn get_sound(&self, event: SoundEvent) -> &EventSound {
        match event {
            SoundEvent::Activity => &self.activity,
            SoundEvent::Error => &self.error,
            SoundEvent::SessionSwap => &self.session_swap,
            SoundEvent::AwaitingInput => &self.awaiting_input,
        }
    }

    pub fn set_sound(&mut self, event: SoundEvent, sound: EventSound) {
        match event {
            SoundEvent::Activity => self.activity = sound,
            SoundEvent::Error => self.error = sound,
            SoundEvent::SessionSwap => self.session_swap = sound,
            SoundEvent::AwaitingInput => self.awaiting_input = sound,
        }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            activity: EventSound {
                attack: 0.01,
                decay: 0.1,
                sustain: 0.1,
                release: 0.2,
                frequency: 440.0,
                waveform: Waveform::Sine,
                volume: 0.3,
            },
            error: EventSound {
                attack: 0.01,
                decay: 0.2,
                sustain: 0.3,
                release: 0.3,
                frequency: 220.0,
                waveform: Waveform::Square,
                volume: 0.4,
            },
            session_swap: EventSound {
                attack: 0.05,
                decay: 0.15,
                sustain: 0.2,
                release: 0.25,
                frequency: 660.0,
                waveform: Waveform::Triangle,
                volume: 0.3,
            },
            awaiting_input: EventSound {
                attack: 0.005,
                decay: 0.08,
                sustain: 0.1,
                release: 0.15,
                frequency: 880.0,
                waveform: Waveform::Sine,
                volume: 0.4,
            },
        }
    }
}

fn load_config() -> Option<SoundConfig> {
    let json = crate::utils::storage_get(STORAGE_KEY)?;
    serde_json::from_str(&json).ok()
}

/// Ensure the AudioContext exists, creating it if needed.
/// Call this from user-gesture handlers (clicks, key presses) to avoid
/// the browser's autoplay policy blocking AudioContext creation.
pub fn ensure_audio_context() {
    AUDIO_CTX.with(|ctx_cell| {
        let mut ctx_ref = ctx_cell.borrow_mut();
        if ctx_ref.is_none() {
            ctx_ref.clone_from(&AudioContext::new().ok());
        }
        if let Some(ref ctx) = *ctx_ref {
            if ctx.state() == AudioContextState::Suspended {
                let _ = ctx.resume();
            }
        }
    });
}

fn get_context() -> Option<AudioContext> {
    AUDIO_CTX.with(|ctx_cell| ctx_cell.borrow().clone())
}

fn synthesize(ctx: &AudioContext, sound: &EventSound) {
    let osc = match ctx.create_oscillator() {
        Ok(o) => o,
        Err(_) => return,
    };
    let gain = match ctx.create_gain() {
        Ok(g) => g,
        Err(_) => return,
    };

    let _ = osc.connect_with_audio_node(&gain);
    let _ = gain.connect_with_audio_node(&ctx.destination());

    osc.frequency().set_value(sound.frequency as f32);
    osc.set_type(sound.waveform.to_oscillator_type());

    let t = ctx.current_time();
    let gain_param = gain.gain();

    // ADSR envelope
    let _ = gain_param.set_value_at_time(0.0, t);
    let _ = gain_param.linear_ramp_to_value_at_time(sound.volume as f32, t + sound.attack);
    let sustain_level = (sound.sustain * sound.volume) as f32;
    let _ = gain_param.linear_ramp_to_value_at_time(sustain_level, t + sound.attack + sound.decay);
    let release_start = t + sound.attack + sound.decay + 0.05;
    let _ = gain_param.linear_ramp_to_value_at_time(0.0, release_start + sound.release);

    let stop_time = release_start + sound.release + 0.01;
    let _ = osc.start();
    let _ = osc.stop_with_when(stop_time);
}

pub fn play_sound(event: SoundEvent) {
    let config = match load_config() {
        Some(c) if c.enabled => c,
        _ => return,
    };

    let now = js_sys::Date::now();
    let cooldown = match event {
        SoundEvent::Activity => ACTIVITY_COOLDOWN_MS,
        _ => DEFAULT_COOLDOWN_MS,
    };
    let throttled = LAST_PLAYED.with(|lp| {
        let map = lp.borrow();
        map.get(&event).is_some_and(|&last| now - last < cooldown)
    });
    if throttled {
        return;
    }
    LAST_PLAYED.with(|lp| {
        lp.borrow_mut().insert(event, now);
    });

    let ctx = match get_context() {
        Some(c) => c,
        None => return, // No AudioContext yet (no user gesture)
    };

    let sound = config.get_sound(event);
    synthesize(&ctx, sound);
}

pub fn play_preview(sound: &EventSound) {
    ensure_audio_context();
    let ctx = match get_context() {
        Some(c) => c,
        None => return,
    };
    synthesize(&ctx, sound);
}
