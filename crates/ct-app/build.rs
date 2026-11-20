//! Compiles the Slint UI. `ui/app.slint` is the single entry point; it re-exports the
//! components of the other `ui/*.slint` files so one `include_modules!()` sees them all.

fn main() {
    if let Err(e) = slint_build::compile("ui/app.slint") {
        panic!("slint compile failed: {e}");
    }
}
