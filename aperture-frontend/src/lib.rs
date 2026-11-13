//! Standalone public exhibit. No Portal API, auth, or shared protocol dependency.
#[path = "../../frontend/src/pages/aperture/mod.rs"]
mod aperture;

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run() {
    yew::Renderer::<aperture::AperturePage>::new().render();
}
