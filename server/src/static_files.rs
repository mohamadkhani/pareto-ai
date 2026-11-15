//! Embedded Angular build, served from memory.
//!
//! The UI is compiled into the binary so production is a single artifact.
//! Build the UI first (`cd web && npm run build`); the build script refuses to
//! compile without it so a broken deploy cannot ship an empty shell.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../web/dist/web/browser"]
struct UiAssets;

/// Serves the Angular app: hashed assets directly, every other path falls back
/// to `index.html` so client-side routes work after a page reload.
///
/// This is the router fallback, so the request may carry no path parameters:
/// the path is read from the URI directly.
pub async fn serve(request: axum::extract::Request) -> Response {
    let requested = request.uri().path().trim_start_matches('/').to_string();

    // A path with no file extension is an Angular route, not a missing asset.
    let is_app_route = requested.is_empty() || !requested.contains('.');
    if is_app_route {
        return serve_index();
    }

    match UiAssets::get(&requested) {
        Some(file) => {
            let mime = mime_guess::from_path(&requested)
                .first_or_octet_stream()
                .essence_str()
                .to_string();
            let cache = cache_control(&requested);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
                file.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

fn serve_index() -> Response {
    match UiAssets::get("index.html") {
        // The shell must never be cached: its asset hashes change on deploy.
        Some(file) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    mime_guess::from_path("index.html")
                        .first_or_octet_stream()
                        .essence_str()
                        .to_string(),
                ),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            file.data,
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "UI build missing; run `cd web && npm run build`",
        )
            .into_response(),
    }
}

/// Angular emits content-hashed names (`main-GFV6MIKA.js`); those are immutable.
/// Anything else changes on deploy and gets a short cache.
///
/// The hash is Angular's base-36-ish suffix, so the stem is checked for length
/// and alphanumeric characters, not hex digits.
fn cache_control(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let hashed = name
        .split_once('-')
        .and_then(|(_, rest)| rest.rsplit_once('.'))
        .is_some_and(|(stem, _)| {
            stem.len() >= 8 && stem.chars().all(|c| c.is_ascii_alphanumeric())
        });

    match hashed {
        true => "public, max-age=31536000, immutable".to_string(),
        false => "public, max-age=3600".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_assets_are_immutable() {
        // Real names from `ng build`, whose hash is alphanumeric but not hex.
        assert_eq!(
            cache_control("main-GFV6MIKA.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control("styles-5INURTSO.css"),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn unhashed_assets_expire_quickly() {
        assert_eq!(cache_control("favicon.ico"), "public, max-age=3600");
        assert_eq!(cache_control("index.html"), "public, max-age=3600");
    }
}
