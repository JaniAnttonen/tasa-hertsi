//! Canvas2D drawing for the live output spectrum (monochrome, no glow).
//!
//! Bars are drawn with a vertical white→dark-gray gradient: that gives
//! depth without adding any color or shadowBlur. Cutoff line and shift
//! arrows are crisp white strokes.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::audio::Audio;
use crate::dsp::engine::EngineMode;
use crate::dsp::spectral::SpecMode;
use crate::dsp::Params;

pub fn start_status_loop<F: FnMut() + 'static>(mut tick: F) {
    let f = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move || {
        tick();
        request_animation_frame(f.borrow().as_ref().unwrap());
    }));
    request_animation_frame(g.borrow().as_ref().unwrap());
}

pub fn start_canvas_loop(
    canvas: HtmlCanvasElement,
    audio: Rc<RefCell<Audio>>,
    sample_rate: f32,
    fft_size: usize,
) {
    let mut spectrum = vec![0.0f32; fft_size / 2];
    let f = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move || {
        let dpr = web_sys::window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0)
            .max(1.0);
        let cw = canvas.client_width().max(1) as f64;
        let ch = canvas.client_height().max(1) as f64;
        let want_w = (cw * dpr) as u32;
        let want_h = (ch * dpr) as u32;
        if canvas.width() != want_w {
            canvas.set_width(want_w);
        }
        if canvas.height() != want_h {
            canvas.set_height(want_h);
        }

        let ctx_opt = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|o| o.dyn_into::<CanvasRenderingContext2d>().ok());

        if let Some(ctx) = ctx_opt {
            audio.borrow().read_spectrum(&mut spectrum);
            let p = audio.borrow().params.borrow().clone();
            ctx.save();
            let _ = ctx.scale(dpr, dpr);
            draw(&ctx, cw, ch, &spectrum, &p, sample_rate, fft_size);
            ctx.restore();
        }

        request_animation_frame(f.borrow().as_ref().unwrap());
    }));
    request_animation_frame(g.borrow().as_ref().unwrap());
}

fn request_animation_frame(closure: &Closure<dyn FnMut()>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(closure.as_ref().unchecked_ref());
    }
}

fn set_fill(ctx: &CanvasRenderingContext2d, color: &str) {
    ctx.set_fill_style(&JsValue::from_str(color));
}
fn set_stroke(ctx: &CanvasRenderingContext2d, color: &str) {
    ctx.set_stroke_style(&JsValue::from_str(color));
}

fn draw(
    ctx: &CanvasRenderingContext2d,
    w: f64,
    h: f64,
    spec: &[f32],
    p: &Params,
    sr: f32,
    fft_size: usize,
) {
    // Background — solid black.
    set_fill(ctx, "#000000");
    ctx.fill_rect(0.0, 0.0, w, h);

    let inset = 14.0_f64;
    let inner_x = inset;
    let inner_y = inset;
    let inner_w = (w - inset * 2.0).max(1.0);
    let inner_h = (h - inset * 2.0).max(1.0);

    let f_min = 30.0_f32;
    let f_max = (sr * 0.5).min(20_000.0);
    let log_min = f_min.log10();
    let log_max = f_max.log10();
    let bin_to_hz = sr / fft_size as f32;

    let f_to_x = |f: f32| -> f64 {
        let lf = f.max(0.5).log10();
        let frac = ((lf - log_min) / (log_max - log_min)).clamp(0.0, 1.0) as f64;
        inner_x + frac * inner_w
    };

    let (active_lo, active_hi, shift_hz) = match p.mode {
        EngineMode::Ssb => (f_min, f_max, p.ssb.shift_hz),
        EngineMode::Spectral => match p.spec.mode {
            SpecMode::Linear => (f_min, f_max, p.spec.shift_hz),
            SpecMode::Highpass => (p.spec.cutoff_hz, f_max, p.spec.shift_hz),
            SpecMode::Lowpass => (f_min, p.spec.cutoff_hz, p.spec.shift_hz),
        },
    };

    // Active region tint — flat, low-alpha white.
    let xa = f_to_x(active_lo.max(f_min));
    let xb = f_to_x(active_hi.min(f_max));
    if xb > xa {
        set_fill(ctx, "rgba(255, 255, 255, 0.05)");
        ctx.fill_rect(xa, inner_y, xb - xa, inner_h);
    }

    // Decade gridlines + Hz tick labels
    ctx.set_line_width(1.0);
    set_stroke(ctx, "rgba(255, 255, 255, 0.08)");
    ctx.set_font("10px ui-monospace, SFMono-Regular, Menlo, monospace");
    set_fill(ctx, "rgba(180, 180, 180, 0.55)");
    for &fline in &[100.0_f32, 1000.0, 10_000.0] {
        if fline > f_max {
            continue;
        }
        let x = f_to_x(fline);
        ctx.begin_path();
        ctx.move_to(x, inner_y);
        ctx.line_to(x, inner_y + inner_h);
        ctx.stroke();
        let label = if fline >= 1000.0 {
            format!("{:.0}k", fline / 1000.0)
        } else {
            format!("{:.0}", fline)
        };
        let _ = ctx.fill_text(&label, x + 4.0, inner_y + inner_h - 4.0);
    }

    // Spectrum bars — solid white, height encodes amplitude.
    set_fill(ctx, "#ffffff");
    let n_bars = ((inner_w / 3.0) as usize).clamp(64, 384);
    let bar_w = inner_w / n_bars as f64;
    for i in 0..n_bars {
        let frac = i as f64 / n_bars as f64;
        let f = 10f32.powf(log_min + frac as f32 * (log_max - log_min));
        let bin = (f / bin_to_hz).clamp(0.0, (spec.len() - 1) as f32) as usize;
        let v = spec[bin].sqrt();
        if v < 0.005 {
            continue;
        }
        let bar_h = (v as f64) * inner_h;
        let x0 = inner_x + frac * inner_w;
        let bar_top = inner_y + inner_h - bar_h;
        ctx.fill_rect(x0, bar_top, (bar_w * 0.85).max(1.0), bar_h);
    }

    // Cutoff line for HP/LP — solid white.
    if p.mode == EngineMode::Spectral
        && (p.spec.mode == SpecMode::Highpass || p.spec.mode == SpecMode::Lowpass)
    {
        let cx = f_to_x(p.spec.cutoff_hz);
        ctx.set_line_width(1.4);
        set_stroke(ctx, "#ffffff");
        ctx.begin_path();
        ctx.move_to(cx, inner_y);
        ctx.line_to(cx, inner_y + inner_h);
        ctx.stroke();

        ctx.set_font("11px ui-sans-serif, system-ui, sans-serif");
        set_fill(ctx, "#ffffff");
        let arrow = if p.spec.mode == SpecMode::Highpass {
            "▶"
        } else {
            "◀"
        };
        let dir: f64 = if p.spec.mode == SpecMode::Highpass {
            6.0
        } else {
            -6.0
        };
        let _ = ctx.set_text_align(if dir > 0.0 { "left" } else { "right" });
        let _ = ctx.fill_text(
            &format!("cutoff {} {:.0} Hz", arrow, p.spec.cutoff_hz),
            cx + dir,
            inner_y + 16.0,
        );
        let _ = ctx.set_text_align("left");
    }

    // Shift indicator: arrows at 100 Hz / 1 kHz / 10 kHz, white.
    if shift_hz.abs() > 1.0 {
        let y = inner_y + 30.0;
        set_stroke(ctx, "#ffffff");
        ctx.set_line_width(1.4);

        for &ref_hz in &[100.0_f32, 1000.0, 10_000.0] {
            if ref_hz < active_lo - 0.01 || ref_hz > active_hi + 0.01 {
                continue;
            }
            if ref_hz > f_max {
                continue;
            }
            let target = ref_hz + shift_hz;
            if target <= f_min || target >= f_max {
                continue;
            }
            let xa = f_to_x(ref_hz);
            let xb = f_to_x(target);

            ctx.begin_path();
            ctx.move_to(xa, y - 4.0);
            ctx.line_to(xa, y + 4.0);
            ctx.stroke();
            ctx.begin_path();
            ctx.move_to(xa, y);
            ctx.line_to(xb, y);
            ctx.stroke();
            let dir = (xb - xa).signum();
            ctx.begin_path();
            ctx.move_to(xb, y);
            ctx.line_to(xb - 6.0 * dir, y - 4.0);
            ctx.move_to(xb, y);
            ctx.line_to(xb - 6.0 * dir, y + 4.0);
            ctx.stroke();
        }

        // Centered shift label
        let center_x = (f_to_x(active_lo.max(f_min)) + f_to_x(active_hi.min(f_max))) * 0.5;
        ctx.set_font("11px ui-sans-serif, system-ui, sans-serif");
        let _ = ctx.set_text_align("center");
        set_fill(ctx, "#ffffff");
        let sign = if shift_hz > 0.0 { "+" } else { "" };
        let _ = ctx.fill_text(
            &format!("{}{:.0} Hz   →   100 / 1k / 10k", sign, shift_hz),
            center_x,
            inner_y + 16.0,
        );
        let _ = ctx.set_text_align("left");
    }
}
