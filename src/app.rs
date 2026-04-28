use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::Rc;

use leptos::html::Canvas;
use leptos::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;

use crate::audio::{Audio, SourceKind};
use crate::dsp::engine::EngineMode;
use crate::dsp::spectral::{SpecMode, FFT_SIZE, HOP_OPTIONS};
use crate::spectrum;

#[component]
pub fn App() -> impl IntoView {
    let audio = Rc::new(RefCell::new(
        Audio::new().expect("audio initialization failed"),
    ));
    let sample_rate = audio.borrow().sample_rate();
    let fft_size = audio.borrow().fft_size();
    let init_params = audio.borrow().params.borrow().clone();

    let (params, set_params) = create_signal(init_params);

    // Mirror params → audio's RefCell on every update.
    {
        let audio = audio.clone();
        create_effect(move |_| {
            let p = params.get();
            *audio.borrow().params.borrow_mut() = p;
        });
    }

    // Status / source / peak — refreshed via requestAnimationFrame.
    let (status_msg, set_status_msg) = create_signal(String::from("ready"));
    let (loaded_name, set_loaded_name) = create_signal(None::<String>);
    let (source, set_source) = create_signal(SourceKind::None);
    let (peak, set_peak) = create_signal(0.0f32);
    {
        let audio = audio.clone();
        spectrum::start_status_loop(move || {
            let a = audio.borrow();
            set_status_msg.set(a.status_message());
            set_loaded_name.set(a.loaded_name());
            set_source.set(a.current_source());
            set_peak.set(a.peak());
        });
    }

    // Spectrum canvas — start a RAF draw loop once the canvas is mounted.
    let canvas_ref = create_node_ref::<Canvas>();
    {
        let audio = audio.clone();
        let started = Rc::new(Cell::new(false));
        create_effect(move |_| {
            if started.get() {
                return;
            }
            if let Some(el) = canvas_ref.get() {
                started.set(true);
                let canvas: HtmlCanvasElement = el.deref().clone();
                spectrum::start_canvas_loop(canvas, audio.clone(), sample_rate, fft_size);
            }
        });
    }

    // ----- button handlers -----
    let on_load_file = {
        let audio = audio.clone();
        move |_| audio.borrow().open_file_picker()
    };
    let on_play = {
        let audio = audio.clone();
        move |_| {
            let _ = audio.borrow_mut().play_file();
        }
    };
    let on_stop = {
        let audio = audio.clone();
        move |_| audio.borrow_mut().stop()
    };
    let on_mic = {
        let audio = audio.clone();
        move |_| {
            let cur = audio.borrow().current_source();
            if cur == SourceKind::Mic {
                audio.borrow_mut().stop();
            } else {
                let audio = audio.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    // Two-step so the RefCell isn't held across the .await.
                    let promise = match audio.borrow_mut().begin_mic_request() {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    let stream_js = match wasm_bindgen_futures::JsFuture::from(promise).await {
                        Ok(v) => v,
                        Err(_) => return,
                    };
                    let stream = match stream_js.dyn_into::<web_sys::MediaStream>() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let _ = audio.borrow_mut().attach_mic_stream(stream);
                });
            }
        }
    };

    // ----- derived signals (memoized so a single slider doesn't re-render
    //       on every unrelated param change) -----
    let shift_hz = create_memo(move |_| params.with(|p| p.ssb.shift_hz));
    let stereo_spread = create_memo(move |_| params.with(|p| p.ssb.stereo_spread_hz));
    let upper_sb = create_memo(move |_| params.with(|p| p.ssb.upper_level));
    let lower_sb = create_memo(move |_| params.with(|p| p.ssb.lower_level));
    let feedback = create_memo(move |_| params.with(|p| p.ssb.feedback));
    let ssb_mix = create_memo(move |_| params.with(|p| p.ssb.mix));

    let spec_shift = create_memo(move |_| params.with(|p| p.spec.shift_hz));
    let spec_cutoff = create_memo(move |_| params.with(|p| p.spec.cutoff_hz));
    let spec_mix = create_memo(move |_| params.with(|p| p.spec.mix));
    let spec_mode = create_memo(move |_| params.with(|p| p.spec.mode));
    let hop_size_sig = create_memo(move |_| params.with(|p| p.spec.hop_size));

    let engine_mode = create_memo(move |_| params.with(|p| p.mode));
    let output_gain = create_memo(move |_| params.with(|p| p.output_gain));

    // Setters for each parameter.
    let set_shift_hz = move |v: f32| set_params.update(|p| p.ssb.shift_hz = v);
    let set_stereo_spread = move |v: f32| set_params.update(|p| p.ssb.stereo_spread_hz = v);
    let set_upper_sb = move |v: f32| set_params.update(|p| p.ssb.upper_level = v);
    let set_lower_sb = move |v: f32| set_params.update(|p| p.ssb.lower_level = v);
    let set_feedback = move |v: f32| set_params.update(|p| p.ssb.feedback = v);
    let set_ssb_mix = move |v: f32| set_params.update(|p| p.ssb.mix = v);

    let set_spec_shift = move |v: f32| set_params.update(|p| p.spec.shift_hz = v);
    let set_spec_cutoff = move |v: f32| set_params.update(|p| p.spec.cutoff_hz = v);
    let set_spec_mix = move |v: f32| set_params.update(|p| p.spec.mix = v);
    let set_output = move |v: f32| set_params.update(|p| p.output_gain = v);

    // Source-state derived UI bits.
    let has_file = create_memo(move |_| loaded_name.with(|n| n.is_some()));
    let is_file_playing = create_memo(move |_| source.get() == SourceKind::File);
    let is_mic_active = create_memo(move |_| source.get() == SourceKind::Mic);

    let file_name_view = move || match loaded_name.get() {
        Some(n) => view! {
            <span class="file-name">
                {format!("{} · {:.1}s", truncate(&n, 28), 0.0_f32)}
            </span>
        }
        .into_view(),
        None => view! {
            <span class="file-name"><span class="empty">"no file loaded"</span></span>
        }
        .into_view(),
    };

    let peak_pct = move || (peak.get().clamp(0.0, 1.5) / 1.5 * 100.0) as f64;

    view! {
        <div class="app">

            <header class="header">
                <span class="logo">"tasahertsiö"</span>
                <span class="tagline">"linear frequency shifter — SSB + spectral"</span>
                <span class="header-spacer"></span>
                <span class="status-pill">{move || status_msg.get()}</span>
                <span class="sr-pill">{move || format!("{:.0} Hz", sample_rate)}</span>
            </header>

            <div class="panel source-bar">
                <button class="btn" on:click=on_load_file>"Load file…"</button>

                <button class="btn primary"
                    prop:disabled=move || !has_file.get() || is_file_playing.get()
                    on:click=on_play>
                    "Play"
                </button>
                <button class="btn"
                    prop:disabled=move || source.get() == SourceKind::None
                    on:click=on_stop>
                    "Stop"
                </button>

                <span class="divider"></span>

                <button
                    class=move || if is_mic_active.get() { "btn danger" } else { "btn" }
                    on:click=on_mic>
                    {move || if is_mic_active.get() {
                        view! { <><span class="dot"></span>"Mic"</> }.into_view()
                    } else {
                        view! { "Microphone" }.into_view()
                    }}
                </button>

                <span class="divider"></span>

                {file_name_view}

                <span class="peak-meter">
                    <span class="fill"
                        style=move || format!("width: {:.1}%;", peak_pct())>
                    </span>
                </span>
            </div>

            <div class="panel">
                <div class="engine-tabs">
                    <button
                        class=move || if engine_mode.get() == EngineMode::Ssb { "engine-tab active" } else { "engine-tab" }
                        on:click=move |_| set_params.update(|p| p.mode = EngineMode::Ssb)>
                        "SSB · time-domain"
                    </button>
                    <button
                        class=move || if engine_mode.get() == EngineMode::Spectral { "engine-tab active" } else { "engine-tab" }
                        on:click=move |_| set_params.update(|p| p.mode = EngineMode::Spectral)>
                        "Spectral · FFT"
                    </button>
                </div>

                {move || match engine_mode.get() {
                    EngineMode::Ssb => view! {
                        <div>
                            <div class="engine-title">"Bode-style SSB shifter"</div>
                            <div class="engine-sub">
                                "Hilbert FIR → analytic signal → complex modulator. Smooth, low-latency."
                            </div>
                            {slider_row("shift", shift_hz.into(), set_shift_hz, -2000.0, 2000.0, 1.0, " Hz", true, 0)}
                            {slider_row("stereo spread", stereo_spread.into(), set_stereo_spread, 0.0, 2000.0, 1.0, " Hz", false, 0)}
                            {slider_row("upper sb", upper_sb.into(), set_upper_sb, 0.0, 1.0, 0.001, "", false, 3)}
                            {slider_row("lower sb", lower_sb.into(), set_lower_sb, 0.0, 1.0, 0.001, "", false, 3)}
                            {slider_row("feedback", feedback.into(), set_feedback, 0.0, 0.92, 0.001, "", false, 3)}
                            {slider_row("dry / wet", ssb_mix.into(), set_ssb_mix, 0.0, 1.0, 0.001, "", false, 3)}
                        </div>
                    }.into_view(),
                    EngineMode::Spectral => view! {
                        <div>
                            <div class="engine-title">{format!("Spectral shifter (FFT, {}-pt)", FFT_SIZE)}</div>
                            <div class="engine-sub">
                                "STFT + per-bin phase compensation. Smaller hop = smoother & more CPU."
                            </div>

                            <div class="chip-row" style="margin-bottom: 6px;">
                                <span class="chip-label">"profile"</span>
                                {[SpecMode::Linear, SpecMode::Highpass, SpecMode::Lowpass]
                                    .iter().map(|&m| view! {
                                        <button
                                            class=move || if spec_mode.get() == m { "chip active" } else { "chip" }
                                            on:click=move |_| set_params.update(|p| p.spec.mode = m)>
                                            {m.label()}
                                        </button>
                                    }).collect_view()}
                            </div>

                            <div class="chip-row" style="margin-bottom: 8px;">
                                <span class="chip-label">"hop"</span>
                                {HOP_OPTIONS.iter().map(|&h| view! {
                                    <button
                                        class=move || if hop_size_sig.get() == h { "chip active" } else { "chip" }
                                        on:click=move |_| set_params.update(|p| p.spec.hop_size = h)>
                                        {format!("{}", h)}
                                    </button>
                                }).collect_view()}
                                <span class={move || if hop_size_sig.get() > FFT_SIZE / 4 { "chip-info warn" } else { "chip-info" }}>
                                    {move || {
                                        let h = hop_size_sig.get();
                                        let overlap = FFT_SIZE as f32 / h as f32;
                                        let warn = if h > FFT_SIZE / 4 { "  ⚠ aliased OLA" } else { "" };
                                        format!("{:.0}× overlap · {:.1} ms latency{}",
                                            overlap, h as f32 * 1000.0 / sample_rate, warn)
                                    }}
                                </span>
                            </div>

                            {slider_row("shift", spec_shift.into(), set_spec_shift, -2000.0, 2000.0, 1.0, " Hz", true, 0)}
                            {move || (spec_mode.get() != SpecMode::Linear).then(|| {
                                slider_row("cutoff", spec_cutoff.into(), set_spec_cutoff, 30.0, 10_000.0, 1.0, " Hz", false, 0)
                            })}
                            {slider_row("dry / wet", spec_mix.into(), set_spec_mix, 0.0, 1.0, 0.001, "", false, 3)}
                        </div>
                    }.into_view(),
                }}
            </div>

            <div class="spectrum-wrap">
                <span class="spectrum-label">"OUTPUT SPECTRUM · log Hz"</span>
                <canvas node_ref=canvas_ref class="spectrum-canvas"></canvas>
            </div>

            <div class="footer panel">
                {slider_row("output", output_gain.into(), set_output, 0.0, 2.0, 0.001, "", false, 3)}
            </div>
        </div>
    }
}

// slider_row is a thin view helper; bundling these into a struct would just
// add ceremony for one call site each.
#[allow(clippy::too_many_arguments)]
fn slider_row(
    label: &'static str,
    value: Signal<f32>,
    on_input: impl Fn(f32) + 'static + Copy,
    min: f32,
    max: f32,
    step: f32,
    suffix: &'static str,
    bipolar: bool,
    decimals: u8,
) -> impl IntoView {
    let style = move || {
        if bipolar {
            let v = value.get();
            let zero_pct = (-min / (max - min) * 100.0).clamp(0.0, 100.0);
            let pos_pct = ((v - min) / (max - min) * 100.0).clamp(0.0, 100.0);
            let (a, b) = if pos_pct > zero_pct {
                (zero_pct, pos_pct)
            } else {
                (pos_pct, zero_pct)
            };
            format!("--bp-a: {:.2}%; --bp-b: {:.2}%;", a, b)
        } else {
            let pct = ((value.get() - min) / (max - min) * 100.0).clamp(0.0, 100.0);
            format!("--fill-pct: {:.2}%;", pct)
        }
    };
    let class = if bipolar { "slider bipolar" } else { "slider" };
    let val_input_text = move || fmt_value(value.get(), decimals);
    let suffix_trimmed = suffix.trim();
    let has_suffix = !suffix_trimmed.is_empty();

    view! {
        <div class="slider-row">
            <div class="label">{label}</div>
            <input
                type="range"
                class=class
                min=min
                max=max
                step=step
                style=style
                prop:value=move || value.get() as f64
                on:input=move |ev| {
                    let v: f32 = event_target_value(&ev).parse().unwrap_or(0.0);
                    on_input(v);
                }
            />
            <div class="val">
                <input
                    type="number"
                    class="val-input"
                    min=min.to_string()
                    max=max.to_string()
                    step=step.to_string()
                    prop:value=val_input_text
                    on:change=move |ev| {
                        let target = match ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) {
                            Some(t) => t,
                            None => return,
                        };
                        let raw = target.value();
                        match raw.trim().parse::<f32>() {
                            Ok(v) => {
                                let clamped = v.clamp(min, max);
                                on_input(clamped);
                                // Force the displayed value to reflect the
                                // clamped/parsed result (signal write may
                                // be a no-op if unchanged, in which case
                                // prop:value won't fire on its own).
                                target.set_value(&fmt_value(clamped, decimals));
                            }
                            Err(_) => {
                                target.set_value(&fmt_value(value.get(), decimals));
                            }
                        }
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            if let Some(t) = ev.target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            {
                                let _ = t.blur();
                            }
                        }
                    }
                />
                {has_suffix.then(|| view! { <span class="val-suffix">{suffix_trimmed}</span> })}
            </div>
        </div>
    }
}

fn fmt_value(v: f32, decimals: u8) -> String {
    match decimals {
        0 => format!("{:.0}", v),
        1 => format!("{:.1}", v),
        2 => format!("{:.2}", v),
        _ => format!("{:.3}", v),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
