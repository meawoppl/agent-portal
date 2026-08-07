//! Classifier / grouping tests, one module per message family so a renderer
//! change only pulls in the family it touches. Wire-shape builders shared by
//! the families live in [`fixtures`].

mod assistant;
mod classifier;
mod codex;
mod fixtures;
mod model_names;
mod muse;
mod portal;
mod thinking;
mod user;
