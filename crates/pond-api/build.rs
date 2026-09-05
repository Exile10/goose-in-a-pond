//! Ensures `pond-desktop/dist` exists at compile time so the `include_dir!`
//! embedding of the web UI (see `routes.rs`) never fails on a fresh checkout. An
//! unbuilt UI gets a placeholder `index.html`, which the handler detects by its
//! `data-giap-placeholder` marker; build the UI before a release build.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pond-desktop/dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        println!("cargo:warning=could not create {}: {e}", dist.display());
        return;
    }
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::write(
            &index,
            "<!doctype html><html data-giap-placeholder><head><meta charset=\"utf-8\">\
             <title>Goose In A Pond</title></head><body>\
             <p>Web UI not built into this binary. Build it with \
             <code>cd pond-desktop &amp;&amp; npm run build</code> then rebuild, \
             or run the server with <code>--static-dir pond-desktop/dist</code>.</p>\
             </body></html>",
        );
    }
    // Re-embed when the built assets change.
    println!("cargo:rerun-if-changed=../../pond-desktop/dist");
    println!("cargo:rerun-if-changed=build.rs");
}
