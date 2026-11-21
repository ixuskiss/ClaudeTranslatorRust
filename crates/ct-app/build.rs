//! Compiles the Slint UI. `ui/app.slint` is the single entry point; it re-exports the
//! components of the other `ui/*.slint` files so one `include_modules!()` sees them all.
//!
//! The style is fixed to `fluent-light` so every window uses the light palette regardless
//! of the OS theme (v5 §4.1); Slint has no runtime API to set the color scheme globally.

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".into());
    if let Err(e) = slint_build::compile_with_config("ui/app.slint", config) {
        panic!("slint compile failed: {e}");
    }
    // Exe icon (Explorer, taskbar, Alt+Tab): the same speech-bubble drawing as the tray icon.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=assets/app.ico");
        if let Err(e) = embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional()
        {
            panic!("embedding Windows resources failed: {e}");
        }
    }
}
