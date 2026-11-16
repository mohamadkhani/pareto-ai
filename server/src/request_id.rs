//! Request identity plumbing: a server-assigned `x-request-id` and the
//! client's optional echo.
//!
//! [`stack`] wraps the router with three layers, in this request order:
//!
//! 1. [`strip`] moves a client-supplied `x-request-id` into a
//!    [`ClientRequestId`] extension and removes the header;
//! 2. `SetRequestIdLayer` stamps a fresh server-generated UUID (the middleware
//!    only stamps when the header is absent, so stripping first makes the id
//!    **always** server-chosen — review blocker B1: a client-controlled ledger
//!    key let a duplicate `x-request-id` roll back both accounting rows);
//! 3. `PropagateRequestIdLayer` copies the stamped id onto the response.
//!
//! Clients therefore see the server id in error bodies and in the response
//! `x-request-id` header; their echo survives only in the ledger's
//! `client_request_id` column.

use axum::Router;
use axum::extract::Request;
use axum::http::{Extensions, HeaderName};
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

/// The client's own `x-request-id` value, preserved by [`strip`] before the
/// middleware stamps the server id.
#[derive(Debug, Clone)]
pub struct ClientRequestId(String);

/// The client's `x-request-id` echo, when it sent one. Stored only in the
/// ledger's `client_request_id` column; never echoed back and never a key.
pub fn client_request_id(extensions: &Extensions) -> Option<String> {
    extensions.get::<ClientRequestId>().map(|id| id.0.clone())
}

/// Moves a client-supplied `x-request-id` into [`ClientRequestId`] so the
/// request-id middleware cannot adopt it. Non-UTF-8 values are dropped: the
/// echo column is TEXT and the value is advisory telemetry.
async fn strip(mut req: Request, next: Next) -> Response {
    if let Some(value) = req.headers_mut().remove("x-request-id")
        && let Ok(value) = value.to_str()
    {
        req.extensions_mut()
            .insert(ClientRequestId(value.to_owned()));
    }
    next.run(req).await
}

/// Wraps `app` with the request-id stack described in the module docs.
/// Layer order: the last `layer()` call runs first, so the chain below reads
/// inside-out (strip → stamp → propagate).
pub fn stack<S>(app: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let header = HeaderName::from_static("x-request-id");
    app.layer(PropagateRequestIdLayer::new(header.clone()))
        .layer(SetRequestIdLayer::new(header, MakeRequestUuid))
        .layer(middleware::from_fn(strip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, StatusCode};
    use tower::ServiceExt;

    async fn echo_ids(req: Request) -> Response {
        let client = client_request_id(req.extensions()).unwrap_or_else(|| "-".to_string());
        let server = crate::error::request_id(req.headers()).unwrap_or_else(|| "-".to_string());
        Response::builder()
            .status(StatusCode::OK)
            .header("x-client-id", client)
            .header("x-server-id", server)
            .body(Body::empty())
            .expect("response")
    }

    fn get_with_id(id: Option<&str>) -> Request {
        let mut builder = Request::builder().method(Method::GET).uri("/");
        if let Some(id) = id {
            builder = builder.header("x-request-id", id);
        }
        builder.body(Body::empty()).expect("request")
    }

    async fn round_trip(id: Option<&str>) -> (String, String, Option<String>) {
        let app = stack(Router::new().route("/", axum::routing::get(echo_ids)));
        let response = app.oneshot(get_with_id(id)).await.expect("response");
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|value| value.to_str().expect("ascii").to_string())
        };
        (
            header("x-server-id").expect("server id"),
            header("x-client-id").expect("client id"),
            header("x-request-id"),
        )
    }

    #[tokio::test]
    async fn a_client_id_is_preserved_but_never_adopted() {
        let (server, client, echoed) = round_trip(Some("client-echo")).await;
        assert_eq!(client, "client-echo", "the echo reaches the ledger hook");
        assert_ne!(server, "client-echo", "the server id is never the echo");
        assert_eq!(
            echoed.as_deref(),
            Some(server.as_str()),
            "clients see the server id"
        );
    }

    #[tokio::test]
    async fn without_a_client_id_the_server_still_stamps_one() {
        let (server, client, echoed) = round_trip(None).await;
        assert_eq!(client, "-", "no echo when the client sent none");
        assert!(!server.is_empty());
        assert_eq!(echoed.as_deref(), Some(server.as_str()));
    }
}
