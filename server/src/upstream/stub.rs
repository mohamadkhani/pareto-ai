//! A scripted local OpenAI-compatible stub for tests: one canned scenario per
//! `POST /chat/completions` request, recording what it saw and when the client
//! hung up. It listens on a random port on loopback; no external network is
//! involved.
//!
//! Scenarios cover the M3 failure matrix: a normal SSE stream with or without
//! `usage`, slow first byte, a stall mid-stream, a truncated stream, malformed
//! SSE, upstream `429`/`500`/`401` responses, a non-streaming JSON body, and
//! an endless slow drip for the client-disconnect test.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::stream;

/// One canned upstream behaviour, consumed per request in order.
#[derive(Debug, Clone)]
pub enum Scenario {
    /// SSE `data:` events, an optional final `usage` chunk, then `[DONE]`.
    Stream {
        events: Vec<String>,
        usage: Option<serde_json::Value>,
    },
    /// Sleep before anything (status line included) is sent, then run `then`.
    SlowStart {
        delay: Duration,
        then: Box<Scenario>,
    },
    /// Send `after` SSE events, stall `delay`, then finish with `[DONE]`.
    Stall { after: usize, delay: Duration },
    /// Send `after` SSE events and end the stream without `[DONE]`.
    Truncate { after: usize },
    /// Send `after` good SSE events, then a `data:` payload that is not JSON.
    Malformed { after: usize },
    /// A fixed error status with optional headers and body.
    Status {
        code: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// A complete non-streaming JSON response.
    Json { body: String },
    /// Slow endless chunks until the client hangs up (disconnect probe).
    Drip { interval: Duration, chunk: String },
}

/// What one upstream request looked like on the wire.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub path: String,
    /// Lowercased header name -> value, for merge assertions.
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
}

#[derive(Default)]
struct StubState {
    scenarios: Mutex<VecDeque<Scenario>>,
    requests: Mutex<Vec<RecordedRequest>>,
    disconnects: Mutex<Vec<Instant>>,
}

/// A running stub: `base_url` is what a provider `base_url` looks like.
pub struct Stub {
    pub base_url: String,
    state: Arc<StubState>,
}

impl Stub {
    /// Starts the stub with the given scenario queue (extra requests get a
    /// `500 no scenario left`, so accidental extra upstream calls are loud).
    pub async fn spawn(scenarios: Vec<Scenario>) -> Self {
        let state = Arc::new(StubState {
            scenarios: Mutex::new(scenarios.into()),
            ..StubState::default()
        });
        let app = Router::new()
            .route("/chat/completions", axum::routing::post(handle))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let addr = listener.local_addr().expect("stub listener address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    /// Requests seen so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn request_count(&self) -> usize {
        self.state
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Instants at which a response body was dropped before completion.
    pub fn disconnects(&self) -> Vec<Instant> {
        self.state
            .disconnects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Polls until a disconnect is recorded, or `budget` elapses. Returns the
    /// recorded instant when one was seen.
    pub async fn wait_for_disconnect(&self, budget: Duration) -> Option<Instant> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(at) = self.disconnects().into_iter().next() {
                return Some(at);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn handle(State(state): State<Arc<StubState>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let headers: BTreeMap<String, String> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .map(|bytes| serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null);

    state
        .requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(RecordedRequest {
            path,
            headers,
            body,
        });

    let scenario = state
        .scenarios
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front()
        .unwrap_or(Scenario::Status {
            code: 500,
            headers: Vec::new(),
            body: r#"{"error":{"message":"no scenario left"}}"#.to_string(),
        });

    respond(state, scenario).await
}

async fn respond(state: Arc<StubState>, scenario: Scenario) -> Response {
    match scenario {
        Scenario::SlowStart { delay, then } => {
            tokio::time::sleep(delay).await;
            // Boxed: `respond` recurses through the `then` scenario.
            Box::pin(respond(state, *then)).await
        }
        Scenario::Status {
            code,
            headers,
            body,
        } => {
            let mut builder = Response::builder()
                .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
                .header(axum::http::header::CONTENT_TYPE, "application/json");
            for (name, value) in headers {
                builder = builder.header(name, value);
            }
            builder.body(Body::from(body)).expect("status response")
        }
        Scenario::Json { body } => Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("json response"),
        Scenario::Stream { events, usage } => {
            let mut lines: Vec<String> = events
                .into_iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect();
            if let Some(usage) = usage {
                // A final chunk carrying `usage`, as OpenAI sends it.
                lines.push(format!(
                    "data: {}\n\n",
                    serde_json::json!({ "usage": usage })
                ));
            }
            lines.push("data: [DONE]\n\n".to_string());
            sse_response(state, chunks(lines, Duration::ZERO))
        }
        Scenario::Stall { after, delay } => {
            let mut lines: Vec<String> = (0..after)
                .map(|i| {
                    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"c{i}\"}}}}]}}\n\n")
                })
                .collect();
            lines.push("data: [DONE]\n\n".to_string());
            let mut chunks = chunks(lines, Duration::ZERO);
            if after > 0 {
                // The stall sits between the last event and the trailing
                // [DONE]; move the delay onto the [DONE] chunk.
                let last = chunks.len() - 1;
                chunks[last].0 = delay;
            }
            sse_response(state, chunks)
        }
        Scenario::Truncate { after } => {
            let lines: Vec<String> = (0..after)
                .map(|i| {
                    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"c{i}\"}}}}]}}\n\n")
                })
                .collect();
            sse_response(state, chunks(lines, Duration::ZERO))
        }
        Scenario::Malformed { after } => {
            let mut lines: Vec<String> = (0..after)
                .map(|i| {
                    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"c{i}\"}}}}]}}\n\n")
                })
                .collect();
            lines.push("data: {this is not json\n\n".to_string());
            sse_response(state, chunks(lines, Duration::ZERO))
        }
        Scenario::Drip { interval, chunk } => {
            let endless = std::iter::repeat_n((interval, chunk), 10_000).collect();
            sse_response(state, endless)
        }
    }
}

type StubChunk = (Duration, String);

fn chunks(lines: Vec<String>, delay: Duration) -> Vec<StubChunk> {
    lines.into_iter().map(|line| (delay, line)).collect()
}

/// Streams `chunks` as the response body. The body holds a guard: if the
/// stream is dropped before its last chunk, the stub records a client
/// disconnect at that instant.
fn sse_response(state: Arc<StubState>, chunks: Vec<StubChunk>) -> Response {
    let guard = DisconnectGuard {
        state,
        finished: false,
    };
    let body_stream = stream::unfold((chunks, guard), |(mut chunks, mut guard)| async move {
        if chunks.is_empty() {
            guard.finish();
            return None;
        }
        let (delay, bytes) = chunks.remove(0);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        Some((Ok::<_, std::io::Error>(bytes), (chunks, guard)))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(body_stream))
        .expect("sse response")
}

/// Records the hang-up instant when dropped mid-stream.
struct DisconnectGuard {
    state: Arc<StubState>,
    finished: bool,
}

impl DisconnectGuard {
    /// Marks the stream as completed so the drop does not record a hang-up.
    fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.state
                .disconnects
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Instant::now());
        }
    }
}
