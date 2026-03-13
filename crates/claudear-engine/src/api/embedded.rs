//! Embedded dashboard assets compiled into the binary via rust-embed.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    response::Response,
};
use rust_embed::Embed;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;

#[derive(Embed)]
#[folder = "../../dashboard/dist/"]
struct DashboardAssets;

/// Returns `true` if the dashboard was embedded at compile time.
pub fn has_dashboard() -> bool {
    DashboardAssets::get("index.html").is_some()
}

/// Axum fallback handler that serves embedded dashboard assets.
///
/// - Exact file matches are served with proper MIME types.
/// - Requests without a file extension that don't start with `/api` are
///   treated as SPA routes and served `index.html`.
/// - Missing assets return 404.
pub fn embedded_fallback(
    req: Request<Body>,
) -> Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>> {
    Box::pin(async move {
        let path = req.uri().path().trim_start_matches('/');

        // Try to serve the exact file
        if let Some(file) = DashboardAssets::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(file.data.to_vec()))
                .unwrap());
        }

        // SPA fallback: serve index.html for non-API, non-asset routes
        let has_extension = path.rsplit('/').next().is_some_and(|s| s.contains('.'));
        if !has_extension && !path.starts_with("api") {
            if let Some(index) = DashboardAssets::get("index.html") {
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html")
                    .body(Body::from(index.data.to_vec()))
                    .unwrap());
            }
        }

        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap())
    })
}
