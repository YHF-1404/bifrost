//! Serve the embedded SPA build output.
//!
//! `web/dist/` is baked into the binary by `rust-embed` at compile
//! time — see `build.rs` for the placeholder fallback when no real
//! build is present.
//!
//! Routing rules:
//!
//! * Exact path matches in the embed → serve that asset with a
//!   content-type from the file extension. Hashed assets under
//!   `/assets/` get an immutable cache; everything else gets `no-cache`.
//! * `/` → serve `index.html`.
//! * Unknown path → SPA fallback: serve `index.html` so the React
//!   router takes over for deep links like `/networks/:nid`.
//! * Embed empty / missing index.html → 503 with a hint to run
//!   `npm run build`.
//!
//! `/api/*` and `/ws` are matched earlier in the router and never
//! reach this fallback.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::{EmbeddedFile, RustEmbed};

// Relative paths are resolved against `CARGO_MANIFEST_DIR` by the
// derive — no `$VAR` interpolation feature needed.
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

const HASHED_ASSETS_PREFIX: &str = "assets/";

pub async fn handler(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let path = if raw.is_empty() { "index.html" } else { raw };

    if let Some(asset) = WebAssets::get(path) {
        return serve(path, asset);
    }

    // SPA fallback. Anything that isn't a recognised asset and isn't
    // an /api or /ws route (those are matched earlier) gets the SPA
    // shell so React Router can handle deep links.
    match WebAssets::get("index.html") {
        Some(asset) => serve("index.html", asset),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "WebUI assets not embedded — run `npm run build` in web/ then rebuild",
        )
            .into_response(),
    }
}

fn serve(path: &str, asset: EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache: HeaderValue = if path.starts_with(HASHED_ASSETS_PREFIX) {
        // Vite emits content-hashed filenames under /assets/, so they
        // are safe to cache forever.
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        // index.html and any other top-level file: revalidate every
        // request so a redeploy is picked up immediately.
        HeaderValue::from_static("no-cache")
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| {
                HeaderValue::from_static("application/octet-stream")
            }),
        )
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(asset.data.into_owned()))
        .unwrap()
}
