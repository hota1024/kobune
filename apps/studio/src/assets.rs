//! The built page, compiled in.
//!
//! One binary, like everything else `apps/` ships. A dashboard that read
//! its own HTML off disk would have a second thing to install and a second
//! thing to get out of step with the daemon it talks to.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::Embed)]
#[folder = "web/dist"]
struct Bundle;

/// Serves a built file, and the shell for anything that is not one.
///
/// The unknown-path fallback is the shell rather than a 404 because the
/// page routes in the browser: a reload on a workspace's URL has to come
/// back as the app, not as a mistake.
pub async fn serve(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');

    // **Nothing under `/api` is ever answered from here.** The guarded
    // router has its own fallback, but this one catches every path the
    // router did not match at all — and `/api/` is such a path, which
    // means without this line a browser asking for it is handed the
    // page by the one handler that sits outside [`crate::guard`]. The
    // API is guarded by where it is, so no shape of URL may wander out
    // of it into the static half.
    if requested == "api" || requested.starts_with("api/") {
        return crate::guard::nowhere().await;
    }

    // **The type comes from what is served, not from what was asked for.**
    // `/` asks for the empty path, which guesses as `application/octet-stream`
    // — and a browser handed HTML under that header downloads it instead of
    // drawing it. Which is the whole page, so it is worth being careful about.
    let (served, file) = match Bundle::get(requested) {
        Some(file) => (requested, file),
        None => match Bundle::get("index.html") {
            Some(file) => ("index.html", file),
            // Only reachable when the bundle was never built.
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "kobune studio was built without its page. Run `pnpm build` in apps/studio/web.",
                )
                    .into_response();
            }
        },
    };

    let mime = mime_guess::from_path(served).first_or_octet_stream();

    // Vite fingerprints everything under /assets/, so those may be kept;
    // the shell names them and must not be.
    let cache = if served.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };

    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        file.data,
    )
        .into_response()
}
