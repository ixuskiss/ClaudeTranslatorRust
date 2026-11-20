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
}
