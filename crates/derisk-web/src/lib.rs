//! The derisk core apps in a browser tab, for demos.
//!
//! Settings, Files, the Text Editor, the System Monitor and the Calculator
//! are the same crates the session links, compiled to WebAssembly and run
//! by eframe's web backend on a canvas. They read and write through
//! derisk-sys, whose demo system stands in for the machine: a home folder in
//! the page's memory and a `/proc` with a made-up LosOS session in it.
//! [`Desktop`] is a small stand-in for the shell around them: a top bar to
//! open apps from and a clock, and a movable window per app.
//!
//! `scripts/build-web.sh` builds the page; on Linux this crate is empty.

#![cfg(target_family = "wasm")]
#![deny(missing_docs)]

mod desktop;

pub use desktop::Desktop;

use wasm_bindgen::{JsCast as _, prelude::wasm_bindgen};

/// Starts the desktop on the page's `<canvas id="derisk">` and takes away
/// its `#loading` note once it runs, or puts the error there if it cannot.
#[wasm_bindgen(start)]
pub fn start() {
    // Already set when the module is started twice; the first one stays.
    let _ = eframe::WebLogger::init(log::LevelFilter::Info);
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(canvas) = document
        .get_element_by_id("derisk")
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
    else {
        log::error!("the page has no <canvas id=\"derisk\">");
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|_| Ok(Box::new(Desktop::new()))),
            )
            .await;
        if let Some(loading) = document.get_element_by_id("loading") {
            match started {
                Ok(()) => loading.remove(),
                Err(error) => {
                    loading.set_text_content(Some(&format!("derisk could not start: {error:?}")));
                }
            }
        }
    });
}
