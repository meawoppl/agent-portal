use crate::audio::{self, EventSound, SoundConfig, SoundEvent, Waveform, STORAGE_KEY};
use crate::utils;
use gloo_net::http::Request;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

const ALL_EVENTS: [SoundEvent; 4] = [
    SoundEvent::Activity,
    SoundEvent::Error,
    SoundEvent::SessionSwap,
    SoundEvent::AwaitingInput,
];

#[function_component(SoundsPanel)]
pub fn sounds_panel() -> Html {
    let config = use_state(SoundConfig::default);
    let dirty = use_state(|| false);
    let saving = use_state(|| false);
    let save_feedback = use_state(|| None::<&'static str>);
    let loading = use_state(|| true);

    // Fetch from API on mount
    {
        let config = config.clone();
        let loading = loading.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let url = utils::api_url("/api/settings/sound");
                if let Ok(resp) = Request::get(&url).send().await {
                    if let Ok(data) = resp.json::<shared::SoundSettingsResponse>().await {
                        if let Some(json) = data.sound_config {
                            if let Ok(cfg) = serde_json::from_value::<SoundConfig>(json) {
                                // Sync to localStorage for play_sound() runtime
                                if let Ok(s) = serde_json::to_string(&cfg) {
                                    if let Some(storage) = web_sys::window()
                                        .and_then(|w| w.local_storage().ok())
                                        .flatten()
                                    {
                                        let _ = storage.set_item(STORAGE_KEY, &s);
                                    }
                                }
                                config.set(cfg);
                            }
                        }
                    }
                }
                loading.set(false);
            });
        });
    }

    let on_change = {
        let config = config.clone();
        let dirty = dirty.clone();
        let save_feedback = save_feedback.clone();
        Callback::from(move |new_config: SoundConfig| {
            config.set(new_config);
            dirty.set(true);
            save_feedback.set(None);
        })
    };

    let on_toggle = {
        let config = config.clone();
        let on_change = on_change.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut cfg = (*config).clone();
            cfg.enabled = input.checked();
            on_change.emit(cfg);
        })
    };

    let on_save = {
        let config = config.clone();
        let dirty = dirty.clone();
        let saving = saving.clone();
        let save_feedback = save_feedback.clone();
        Callback::from(move |_: MouseEvent| {
            let config = config.clone();
            let dirty = dirty.clone();
            let saving = saving.clone();
            let save_feedback = save_feedback.clone();
            spawn_local(async move {
                saving.set(true);
                let cfg = (*config).clone();
                let url = utils::api_url("/api/settings/sound");
                let json = serde_json::to_value(&cfg).unwrap_or_default();
                let result = Request::put(&url)
                    .json(&json)
                    .expect("json body")
                    .send()
                    .await;
                match result {
                    Ok(resp) if resp.ok() => {
                        // Update localStorage so play_sound() uses new settings
                        if let Ok(s) = serde_json::to_string(&cfg) {
                            if let Some(storage) = web_sys::window()
                                .and_then(|w| w.local_storage().ok())
                                .flatten()
                            {
                                let _ = storage.set_item(STORAGE_KEY, &s);
                            }
                        }
                        dirty.set(false);
                        save_feedback.set(Some("Saved!"));
                    }
                    _ => {
                        save_feedback.set(Some("Save failed"));
                    }
                }
                saving.set(false);
            });
        })
    };

    if *loading {
        return html! {
            <section class="sounds-section">
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading sound settings..." }</p>
                </div>
            </section>
        };
    }

    html! {
        <section class="sounds-section">
            <div class="section-header">
                <h2>{ "Sound Notifications" }</h2>
                <p class="section-description">
                    { "Configure synthesized sounds for different events." }
                </p>
                <div class="sound-save-area">
                    if let Some(msg) = *save_feedback {
                        <span class={classes!(
                            "save-feedback",
                            (*dirty).then_some("unsaved"),
                            (!*dirty).then_some("saved"),
                        )}>{ msg }</span>
                    }
                    <button
                        class={classes!("create-button", (!*dirty).then_some("disabled"))}
                        onclick={on_save}
                        disabled={!*dirty || *saving}
                    >
                        { if *saving { "Saving..." } else { "Save" } }
                    </button>
                </div>
            </div>

            <div class="sound-toggle">
                <label class="toggle-label">
                    <span>{ "Enable Sounds" }</span>
                    <input
                        type="checkbox"
                        checked={config.enabled}
                        onchange={on_toggle}
                    />
                </label>
            </div>

            <div class="sound-event-cards">
                { for ALL_EVENTS.iter().map(|&event| {
                    let sound = config.get_sound(event).clone();
                    let config = config.clone();
                    let on_change = on_change.clone();
                    let on_sound_change = Callback::from(move |new_sound: EventSound| {
                        let mut cfg = (*config).clone();
                        cfg.set_sound(event, new_sound);
                        on_change.emit(cfg);
                    });
                    html! {
                        <EventCard
                            key={event.label()}
                            event={event}
                            sound={sound}
                            on_change={on_sound_change}
                        />
                    }
                }) }
            </div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct EventCardProps {
    event: SoundEvent,
    sound: EventSound,
    on_change: Callback<EventSound>,
}

#[function_component(EventCard)]
fn event_card(props: &EventCardProps) -> Html {
    let event = props.event;
    let sound = &props.sound;
    let on_change = &props.on_change;

    let on_preview = {
        let sound = sound.clone();
        Callback::from(move |_: MouseEvent| {
            audio::play_preview(&sound);
        })
    };

    let on_waveform = {
        let sound = sound.clone();
        let on_change = on_change.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let mut s = sound.clone();
            s.waveform = match select.value().as_str() {
                "Square" => Waveform::Square,
                "Sawtooth" => Waveform::Sawtooth,
                "Triangle" => Waveform::Triangle,
                _ => Waveform::Sine,
            };
            on_change.emit(s);
        })
    };

    let make_slider = |label: &str,
                       value: f64,
                       min: f64,
                       max: f64,
                       step: f64,
                       unit: &str,
                       field: &'static str|
     -> Html {
        let sound = sound.clone();
        let on_change = on_change.clone();
        let display = if unit == "Hz" {
            format!("{:.0}{}", value, unit)
        } else {
            format!("{:.3}{}", value, unit)
        };
        let on_input = Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let v: f64 = input.value().parse().unwrap_or(value);
            let mut s = sound.clone();
            match field {
                "attack" => s.attack = v,
                "decay" => s.decay = v,
                "sustain" => s.sustain = v,
                "release" => s.release = v,
                "frequency" => s.frequency = v,
                "volume" => s.volume = v,
                _ => {}
            }
            on_change.emit(s);
        });
        html! {
            <div class="sound-control">
                <label>{ label }</label>
                <input
                    type="range"
                    min={min.to_string()}
                    max={max.to_string()}
                    step={step.to_string()}
                    value={value.to_string()}
                    oninput={on_input}
                />
                <span class="sound-value">{ display }</span>
            </div>
        }
    };

    html! {
        <div class="sound-event-card">
            <div class="sound-card-header">
                <div>
                    <h3>{ event.label() }</h3>
                    <p>{ event.description() }</p>
                </div>
                <button class="preview-button" onclick={on_preview}>
                    { "Preview" }
                </button>
            </div>
            <div class="sound-controls">
                <div class="sound-control">
                    <label>{ "Waveform" }</label>
                    <select onchange={on_waveform}>
                        { for Waveform::all().iter().map(|w| {
                            html! {
                                <option
                                    value={w.label()}
                                    selected={*w == sound.waveform}
                                >
                                    { w.label() }
                                </option>
                            }
                        }) }
                    </select>
                </div>
                { make_slider("Frequency", sound.frequency, 100.0, 2000.0, 10.0, "Hz", "frequency") }
                { make_slider("Attack", sound.attack, 0.001, 1.0, 0.001, "s", "attack") }
                { make_slider("Decay", sound.decay, 0.001, 1.0, 0.001, "s", "decay") }
                { make_slider("Sustain", sound.sustain, 0.0, 1.0, 0.01, "", "sustain") }
                { make_slider("Release", sound.release, 0.001, 2.0, 0.001, "s", "release") }
                { make_slider("Volume", sound.volume, 0.0, 1.0, 0.01, "", "volume") }
            </div>
        </div>
    }
}
