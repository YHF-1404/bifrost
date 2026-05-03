//! Make sure `web/dist/index.html` exists at compile time.
//!
//! `rust-embed` requires its `folder` to exist; if the WebUI hasn't
//! been built yet (fresh clone, CI without Node, etc.) we write a
//! minimal placeholder so cargo still compiles and the runtime gives
//! a useful diagnostic instead of crashing.

use std::fs;
use std::path::PathBuf;

fn main() {
    // CARGO_MANIFEST_DIR = .../crates/bifrost-web; web/ is two levels up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = manifest.join("../../web/dist");
    let index = dist.join("index.html");

    if !index.exists() {
        if let Err(e) = fs::create_dir_all(&dist) {
            // Bail loudly: cargo build will fail anyway when rust-embed
            // can't find the folder. Better to print the real error.
            panic!("create web/dist: {e}");
        }
        let placeholder = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>Bifrost — WebUI not built</title>
  <style>
    body { font-family: ui-sans-serif, system-ui, sans-serif;
           max-width: 38rem; margin: 4rem auto; padding: 0 1rem;
           color: #1e293b; line-height: 1.55; }
    code { background: #f1f5f9; padding: 0.1em 0.4em; border-radius: 4px; }
  </style>
</head>
<body>
  <h1>WebUI not built</h1>
  <p>This is the placeholder shipped when <code>web/dist/</code> was empty
     at <code>bifrost-web</code> compile time.</p>
  <p>To enable the real WebUI:</p>
  <pre>cd web
npm install
npm run build
cargo build --release -p bifrost-server</pre>
</body>
</html>
"#;
        if let Err(e) = fs::write(&index, placeholder) {
            panic!("write placeholder index.html: {e}");
        }
    }

    // Re-run when the dist contents change so a `npm run build` in
    // web/ triggers a re-embed without manual cargo clean.
    println!("cargo:rerun-if-changed=../../web/dist");
}
