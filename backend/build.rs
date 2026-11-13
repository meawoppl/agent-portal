#[path = "../build-support/identity.rs"]
mod identity;

fn main() {
    identity::emit();
    for directory in ["aperture-assets", "aperture-demo"] {
        assert!(
            !std::path::Path::new("../frontend/dist")
                .join(directory)
                .exists(),
            "Aperture media belongs in the standalone Pages build; rebuild frontend/dist"
        );
    }
    memory_serve::load_directory("../frontend/dist");
}
