//! Standalone public exhibit. No Portal API, auth, or shared protocol dependency.
mod aperture;

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run() {
    yew::Renderer::<aperture::AperturePage>::new().render();
}
