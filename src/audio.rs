use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AnalyserNode, AudioBuffer, AudioBufferSourceNode, AudioContext, AudioProcessingEvent, GainNode,
    HtmlInputElement, MediaStream, MediaStreamAudioSourceNode, MediaStreamConstraints,
    ScriptProcessorNode,
};

use crate::dsp::{Engine, Params};

#[derive(Default, Clone, Copy, PartialEq)]
pub enum SourceKind {
    #[default]
    None,
    File,
    Mic,
}

#[derive(Default, Clone)]
pub struct Status {
    pub message: String,
    pub source: SourceKind,
    pub loaded_name: Option<String>,
    pub loaded_duration: f32,
}

/// Owns the Web Audio graph and the Rust DSP engine.
///
/// Graph: `[source] → [ScriptProcessor] → [Analyser] → [Gain] → [Destination]`.
/// The ScriptProcessor's `onaudioprocess` callback (a Rust `Closure`) hands
/// the input buffer to `Engine::process_stereo` and writes the result back
/// to the output buffer.
pub struct Audio {
    ctx: AudioContext,
    script: ScriptProcessorNode,
    analyser: AnalyserNode,
    output_gain: GainNode,

    file_source: Option<AudioBufferSourceNode>,
    mic_source: Option<MediaStreamAudioSourceNode>,
    mic_stream: Option<MediaStream>,

    loaded_buffer: Rc<RefCell<Option<AudioBuffer>>>,
    pub params: Rc<RefCell<Params>>,
    pub peak: Rc<RefCell<f32>>,
    pub status: Rc<RefCell<Status>>,
    sample_rate: f32,
    fft_size: u32,

    _audioprocess_cb: Closure<dyn FnMut(AudioProcessingEvent)>,
    _file_input_cb: Closure<dyn FnMut(web_sys::Event)>,
}

impl Audio {
    pub fn new() -> Result<Self, JsValue> {
        let ctx = AudioContext::new()?;
        let sample_rate = ctx.sample_rate();

        let buffer_size = 2048u32;
        let script = ctx
            .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(
                buffer_size, 2, 2,
            )?;
        let analyser = ctx.create_analyser()?;
        analyser.set_fft_size(2048);
        analyser.set_smoothing_time_constant(0.6);
        analyser.set_min_decibels(-90.0);
        analyser.set_max_decibels(-10.0);
        let output_gain = ctx.create_gain()?;
        output_gain.gain().set_value(1.0);

        script.connect_with_audio_node(&analyser)?;
        analyser.connect_with_audio_node(&output_gain)?;
        output_gain.connect_with_audio_node(&ctx.destination())?;

        let engine = Rc::new(RefCell::new(Engine::new(sample_rate)));
        let params = Rc::new(RefCell::new(Params::default()));
        let peak = Rc::new(RefCell::new(0.0f32));
        let status = Rc::new(RefCell::new(Status::default()));
        let loaded_buffer: Rc<RefCell<Option<AudioBuffer>>> = Rc::new(RefCell::new(None));

        let audioprocess_cb =
            make_audioprocess_callback(engine.clone(), params.clone(), peak.clone());
        script.set_onaudioprocess(Some(audioprocess_cb.as_ref().unchecked_ref()));

        let file_input = lookup_file_input()?;
        let file_input_cb = make_file_input_callback(
            ctx.clone(),
            file_input.clone(),
            loaded_buffer.clone(),
            status.clone(),
        );
        file_input
            .add_event_listener_with_callback("change", file_input_cb.as_ref().unchecked_ref())?;

        Ok(Self {
            ctx,
            script,
            analyser,
            output_gain,
            file_source: None,
            mic_source: None,
            mic_stream: None,
            loaded_buffer,
            params,
            peak,
            status,
            sample_rate,
            fft_size: 2048,
            _audioprocess_cb: audioprocess_cb,
            _file_input_cb: file_input_cb,
        })
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size as usize
    }

    pub fn has_file(&self) -> bool {
        self.loaded_buffer.borrow().is_some()
    }

    pub fn loaded_name(&self) -> Option<String> {
        self.status.borrow().loaded_name.clone()
    }

    pub fn loaded_duration(&self) -> f32 {
        self.status.borrow().loaded_duration
    }

    pub fn current_source(&self) -> SourceKind {
        self.status.borrow().source
    }

    pub fn status_message(&self) -> String {
        self.status.borrow().message.clone()
    }

    pub fn peak(&self) -> f32 {
        *self.peak.borrow()
    }

    pub fn set_output_gain(&self, gain: f32) {
        self.output_gain.gain().set_value(gain);
    }

    /// Trigger the hidden `<input type=file>` so the browser shows a picker.
    pub fn open_file_picker(&self) {
        if let Ok(input) = lookup_file_input() {
            input.click();
        }
    }

    /// Resume the AudioContext — required after a user gesture in most browsers.
    pub fn resume(&self) {
        let _ = self.ctx.resume();
    }

    pub fn play_file(&mut self) -> Result<(), JsValue> {
        self.stop();
        let buf_opt = self.loaded_buffer.borrow().clone();
        let buf = match buf_opt {
            Some(b) => b,
            None => {
                self.status.borrow_mut().message = "no file loaded".into();
                return Ok(());
            }
        };
        self.resume();

        let src = self.ctx.create_buffer_source()?;
        src.set_buffer(Some(&buf));
        src.set_loop(true);
        src.connect_with_audio_node(&self.script)?;
        src.start()?;

        self.file_source = Some(src);
        let mut st = self.status.borrow_mut();
        st.source = SourceKind::File;
        st.message = "playing file".into();
        Ok(())
    }

    /// Stop any current source, resume the AudioContext, and kick off a
    /// `getUserMedia` request. Returns the JS Promise so the caller can
    /// `.await` it without holding a `RefCell` borrow.
    pub fn begin_mic_request(&mut self) -> Result<js_sys::Promise, JsValue> {
        self.stop();
        self.resume();
        let nav = web_sys::window().ok_or("no window")?.navigator();
        let media = nav.media_devices()?;
        let constraints = MediaStreamConstraints::new();
        constraints.set_audio(&JsValue::TRUE);
        constraints.set_video(&JsValue::FALSE);
        media.get_user_media_with_constraints(&constraints)
    }

    /// Plug the resolved `MediaStream` into the audio graph.
    pub fn attach_mic_stream(&mut self, stream: MediaStream) -> Result<(), JsValue> {
        let src = self.ctx.create_media_stream_source(&stream)?;
        src.connect_with_audio_node(&self.script)?;
        self.mic_source = Some(src);
        self.mic_stream = Some(stream);
        let mut st = self.status.borrow_mut();
        st.source = SourceKind::Mic;
        st.message = "microphone live".into();
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(src) = self.file_source.take() {
            // web-sys flagged `stop()` deprecated in 0.3.95 in favour of the
            // typed `stop_with_when(...)`, but the no-arg variant is still
            // the spec-canonical way to stop "now".
            #[allow(deprecated)]
            let _ = src.stop();
            let _ = src.disconnect();
        }
        if let Some(src) = self.mic_source.take() {
            let _ = src.disconnect();
        }
        if let Some(stream) = self.mic_stream.take() {
            let tracks = stream.get_tracks();
            for i in 0..tracks.length() {
                if let Ok(track) = tracks.get(i).dyn_into::<web_sys::MediaStreamTrack>() {
                    track.stop();
                }
            }
        }
        let mut st = self.status.borrow_mut();
        st.source = SourceKind::None;
        st.message = "stopped".into();
    }

    /// Copy the current frequency-domain magnitudes (0..1) into `out`.
    pub fn read_spectrum(&self, out: &mut Vec<f32>) {
        let n = self.analyser.frequency_bin_count() as usize;
        if out.len() != n {
            out.resize(n, 0.0);
        }
        let mut bytes = vec![0u8; n];
        self.analyser.get_byte_frequency_data(&mut bytes);
        for i in 0..n {
            out[i] = bytes[i] as f32 / 255.0;
        }
    }
}

fn lookup_file_input() -> Result<HtmlInputElement, JsValue> {
    let document = web_sys::window()
        .ok_or_else(|| JsValue::from_str("no window"))?
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let el = document
        .get_element_by_id("file-input")
        .ok_or_else(|| JsValue::from_str("missing #file-input"))?;
    el.dyn_into::<HtmlInputElement>()
        .map_err(|_| JsValue::from_str("file-input is not an <input>"))
}

fn make_audioprocess_callback(
    engine: Rc<RefCell<Engine>>,
    params: Rc<RefCell<Params>>,
    peak: Rc<RefCell<f32>>,
) -> Closure<dyn FnMut(AudioProcessingEvent)> {
    // Persistent scratch buffers, captured by the closure (no per-callback alloc).
    let mut in_l: Vec<f32> = Vec::with_capacity(8192);
    let mut in_r: Vec<f32> = Vec::with_capacity(8192);
    let mut out_l: Vec<f32> = Vec::with_capacity(8192);
    let mut out_r: Vec<f32> = Vec::with_capacity(8192);

    Closure::<dyn FnMut(AudioProcessingEvent)>::new(move |ev: AudioProcessingEvent| {
        let in_buf = match ev.input_buffer() {
            Ok(b) => b,
            Err(_) => return,
        };
        let out_buf = match ev.output_buffer() {
            Ok(b) => b,
            Err(_) => return,
        };
        let n = out_buf.length() as usize;
        in_l.resize(n, 0.0);
        in_r.resize(n, 0.0);
        out_l.resize(n, 0.0);
        out_r.resize(n, 0.0);

        let n_in_chans = in_buf.number_of_channels();
        if n_in_chans >= 1 {
            let _ = in_buf.copy_from_channel(&mut in_l, 0);
        }
        if n_in_chans >= 2 {
            let _ = in_buf.copy_from_channel(&mut in_r, 1);
        } else {
            in_r.copy_from_slice(&in_l);
        }

        {
            let mut eng = engine.borrow_mut();
            let p = params.borrow();
            eng.set_sample_rate(out_buf.sample_rate());
            eng.process_stereo(&in_l, &in_r, &mut out_l, &mut out_r, &p);
        }

        let mut pk: f32 = 0.0;
        for s in out_l.iter().chain(out_r.iter()) {
            let a = s.abs();
            if a > pk {
                pk = a;
            }
        }
        {
            let mut p = peak.borrow_mut();
            let decay = 0.92;
            *p = if pk > *p { pk } else { *p * decay };
        }

        let _ = out_buf.copy_to_channel(&out_l, 0);
        let _ = out_buf.copy_to_channel(&out_r, 1);
    })
}

fn make_file_input_callback(
    ctx: AudioContext,
    file_input: HtmlInputElement,
    loaded_buffer: Rc<RefCell<Option<AudioBuffer>>>,
    status: Rc<RefCell<Status>>,
) -> Closure<dyn FnMut(web_sys::Event)> {
    Closure::<dyn FnMut(web_sys::Event)>::new(move |_ev: web_sys::Event| {
        let files = match file_input.files() {
            Some(f) if f.length() > 0 => f,
            _ => return,
        };
        let file = match files.get(0) {
            Some(f) => f,
            None => return,
        };
        let ctx = ctx.clone();
        let loaded = loaded_buffer.clone();
        let status = status.clone();
        wasm_bindgen_futures::spawn_local(async move {
            status.borrow_mut().message = format!("decoding {}…", file.name());
            let arr_js = match JsFuture::from(file.array_buffer()).await {
                Ok(v) => v,
                Err(_) => {
                    status.borrow_mut().message = "failed to read file".into();
                    return;
                }
            };
            let arr: js_sys::ArrayBuffer = match arr_js.dyn_into() {
                Ok(a) => a,
                Err(_) => {
                    status.borrow_mut().message = "not an ArrayBuffer".into();
                    return;
                }
            };
            let promise = match ctx.decode_audio_data(&arr) {
                Ok(p) => p,
                Err(_) => {
                    status.borrow_mut().message = "decode failed".into();
                    return;
                }
            };
            match JsFuture::from(promise).await {
                Ok(buf_js) => {
                    let buf: AudioBuffer = match buf_js.dyn_into() {
                        Ok(b) => b,
                        Err(_) => {
                            status.borrow_mut().message = "decoded value not AudioBuffer".into();
                            return;
                        }
                    };
                    let mut st = status.borrow_mut();
                    st.loaded_name = Some(file.name());
                    st.loaded_duration = buf.duration() as f32;
                    st.message = format!("loaded ({:.1}s) — press play", buf.duration());
                    *loaded.borrow_mut() = Some(buf);
                }
                Err(_) => status.borrow_mut().message = "decode failed".into(),
            }
        });
    })
}
