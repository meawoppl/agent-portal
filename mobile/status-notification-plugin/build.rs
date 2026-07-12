const COMMANDS: &[&str] = &["show", "clear"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
