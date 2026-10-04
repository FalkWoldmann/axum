//! In-process request benchmarks.
//!
//! Unlike `benches.rs`, which load tests a real server with `rewrk`, these call the `Router`
//! directly so small per-request changes are measurable. Run with `cargo bench --bench requests`.

#![allow(missing_docs)]

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header::CONTENT_TYPE, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, Response},
    routing::{get, post},
    Extension, Form, Json, Router,
};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::hint::black_box;
use tower::ServiceExt;

#[derive(Clone)]
struct AppState {
    _string: String,
    _vec: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct Payload {
    n: u32,
    s: String,
    b: bool,
}

#[derive(Deserialize)]
struct Pagination {
    offset: Option<usize>,
    limit: Option<usize>,
}

fn app_state() -> AppState {
    AppState {
        _string: "aaaaaaaaaaaaaaaaaa".to_owned(),
        _vec: Vec::from([
            "aaaaaaaaaaaaaaaaaa".to_owned(),
            "bbbbbbbbbbbbbbbbbb".to_owned(),
            "cccccccccccccccccc".to_owned(),
        ]),
    }
}

struct Case {
    status: StatusCode,
    method: Method,
    uri: &'static str,
    content_type: Option<&'static str>,
    body: &'static str,
}

impl Case {
    fn get(uri: &'static str) -> Self {
        Self {
            status: StatusCode::OK,
            method: Method::GET,
            uri,
            content_type: None,
            body: "",
        }
    }

    fn post(uri: &'static str, content_type: &'static str, body: &'static str) -> Self {
        Self {
            status: StatusCode::OK,
            method: Method::POST,
            uri,
            content_type: Some(content_type),
            body,
        }
    }

    fn status(self, status: StatusCode) -> Self {
        Self { status, ..self }
    }

    fn request(&self) -> Request {
        let mut builder = Request::builder().method(self.method.clone()).uri(self.uri);
        if let Some(content_type) = self.content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        builder.body(Body::from(self.body)).unwrap()
    }
}

fn bench(c: &mut Criterion, name: &str, app: Router, case: &Case) {
    // `axum::serve` calls `with_state(())` once before handling requests, so do the same
    let app = app.with_state(());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    // make sure the benchmark measures the intended path rather than e.g. a rejection
    let status = rt
        .block_on(app.clone().oneshot(case.request()))
        .unwrap()
        .status();
    assert_eq!(status, case.status, "unexpected status for {name}");

    c.bench_function(name, |b| {
        b.to_async(&rt).iter_batched(
            || case.request(),
            |req| async {
                let res = app.clone().oneshot(req).await.unwrap();
                drain(res).await;
            },
            BatchSize::SmallInput,
        );
    });
}

// Poll the body to the end like hyper would, without collecting it into a new buffer.
async fn drain(res: Response) {
    let mut body = res.into_body();
    while let Some(frame) = body.frame().await {
        black_box(frame.unwrap());
    }
}

fn requests(c: &mut Criterion) {
    bench(
        c,
        "minimal",
        Router::new(),
        &Case::get("/").status(StatusCode::NOT_FOUND),
    );

    bench(
        c,
        "basic",
        Router::new().route("/a/b/c", get(|| async { "Hello, World!" })),
        &Case::get("/a/b/c"),
    );

    bench(
        c,
        "basic-merge",
        Router::new().merge(Router::new().route("/a/b/c", get(|| async { "Hello, World!" }))),
        &Case::get("/a/b/c"),
    );

    bench(
        c,
        "basic-nest",
        Router::new().nest(
            "/a",
            Router::new().nest(
                "/b",
                Router::new().route("/c", get(|| async { "Hello, World!" })),
            ),
        ),
        &Case::get("/a/b/c"),
    );

    let mut routing = Router::new();
    for a in 0..10 {
        for b in 0..10 {
            for c in 0..10 {
                routing = routing.route(&format!("/foo-{a}/bar-{b}/baz-{c}"), get(|| async {}));
            }
        }
    }
    bench(
        c,
        "routing",
        routing.route("/foo/bar/baz", get(|| async {})),
        &Case::get("/foo/bar/baz"),
    );

    bench(
        c,
        "method-not-allowed",
        Router::new().route("/", get(|| async {})),
        &Case::post("/", "text/plain", "").status(StatusCode::METHOD_NOT_ALLOWED),
    );

    bench(
        c,
        "html",
        Router::new().route("/", get(|| async { Html("<h1>Hello, World!</h1>") })),
        &Case::get("/"),
    );

    bench(
        c,
        "receive-json",
        Router::new().route("/", post(|_: Json<Payload>| async {})),
        &Case::post(
            "/",
            "application/json",
            r#"{"n": 123, "s": "hi there", "b": false}"#,
        ),
    );

    bench(
        c,
        "send-json",
        Router::new().route(
            "/",
            get(|| async {
                Json(Payload {
                    n: 123,
                    s: "hi there".to_owned(),
                    b: false,
                })
            }),
        ),
        &Case::get("/"),
    );

    bench(
        c,
        "echo-json",
        Router::new().route(
            "/",
            post(|Json(payload): Json<Payload>| async { Json(payload) }),
        ),
        &Case::post(
            "/",
            "application/json",
            r#"{"n": 123, "s": "hi there", "b": false}"#,
        ),
    );

    bench(
        c,
        "receive-form",
        Router::new().route("/", post(|_: Form<Payload>| async {})),
        &Case::post(
            "/",
            "application/x-www-form-urlencoded",
            "n=123&s=hi+there&b=false",
        ),
    );

    bench(
        c,
        "query",
        Router::new().route(
            "/",
            get(|Query(p): Query<Pagination>| async move {
                black_box((p.offset, p.limit));
            }),
        ),
        &Case::get("/?offset=10&limit=20"),
    );

    bench(
        c,
        "path",
        Router::new().route(
            "/users/{id}/posts/{slug}",
            get(|Path((id, slug)): Path<(u32, String)>| async move {
                black_box((id, slug));
            }),
        ),
        &Case::get("/users/42/posts/hello-world"),
    );

    bench(
        c,
        "extension",
        Router::new()
            .route("/", get(|_: Extension<AppState>| async {}))
            .layer(Extension(app_state())),
        &Case::get("/"),
    );

    bench(
        c,
        "state",
        Router::new()
            .route("/", get(|_: State<AppState>| async {}))
            .with_state(app_state()),
        &Case::get("/"),
    );

    async fn passthrough(req: Request, next: Next) -> Response {
        next.run(req).await
    }
    bench(
        c,
        "from-fn",
        Router::new()
            .route("/", get(|| async { "Hello, World!" }))
            .layer(middleware::from_fn(passthrough)),
        &Case::get("/"),
    );
}

criterion_group!(benches, requests);
criterion_main!(benches);
