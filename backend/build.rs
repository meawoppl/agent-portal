#[path = "../build-support/identity.rs"]
mod identity;

fn main() {
    identity::emit();
    memory_serve::load_directory("../frontend/dist");
}
