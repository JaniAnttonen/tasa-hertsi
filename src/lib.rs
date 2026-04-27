pub mod app;
pub mod audio;
pub mod dsp;
pub mod spectrum;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    leptos::mount_to_body(app::App);
}
