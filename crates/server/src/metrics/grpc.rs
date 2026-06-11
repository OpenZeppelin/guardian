//! gRPC request-path instrumentation.
//!
//! A tower layer applied to the tonic server. The wrinkle relative to
//! HTTP: gRPC carries its status code in HTTP/2 *trailers* for normal
//! calls (the response headers say 200 long before the call finishes),
//! and in the response *headers* only for trailers-only error
//! responses. So the response body is wrapped and the request is
//! recorded when the trailer frame is observed — falling back to the
//! header status when the stream ends without trailers, and to
//! `cancelled` when the client drops the response mid-stream.
//!
//! Labels are bounded: `service`/`method` come from the proto
//! definition (unknown shapes collapse into `unknown`), `code` is the
//! closed 17-value gRPC status set.

use http_body::{Body, Frame};
use metrics::{counter, histogram};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tower::{Layer, Service};

use super::names::{
    GRPC_REQUEST_DURATION_SECONDS, GRPC_REQUESTS_TOTAL, LABEL_CODE, LABEL_METHOD, LABEL_SERVICE,
    grpc_code_label, normalize_grpc_method,
};

/// gRPC status code recorded when a response body is dropped before
/// its trailers arrive (client cancellation / disconnect).
const CODE_CANCELLED: i32 = 1;

/// Tower layer recording per-request gRPC metrics. With
/// `enabled = false` the layer still wraps (the service type is
/// fixed at build time) but skips all measurement work.
#[derive(Debug, Clone)]
pub struct MetricsGrpcLayer {
    enabled: bool,
}

impl MetricsGrpcLayer {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl<S> Layer<S> for MetricsGrpcLayer {
    type Service = MetricsGrpcService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsGrpcService {
            inner,
            enabled: self.enabled,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsGrpcService<S> {
    inner: S,
    enabled: bool,
}

impl<S, ReqBody, ResBody> Service<http::Request<ReqBody>> for MetricsGrpcService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Future: Send + 'static,
    ResBody: Body,
{
    type Response = http::Response<MetricsGrpcBody<ResBody>>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<ReqBody>) -> Self::Future {
        let tracker = self.enabled.then(|| {
            let (service, method) = normalize_grpc_method(request.uri().path());
            GrpcRequestTracker {
                service: service.to_owned(),
                method: method.to_owned(),
                started: Instant::now(),
                header_code: None,
            }
        });

        let future = self.inner.call(request);
        Box::pin(async move {
            let response = future.await?;

            let tracker = tracker.map(|mut tracker| {
                // Trailers-only responses (immediate errors) surface
                // grpc-status in the headers; remember it as the
                // fallback for streams that end without trailers.
                tracker.header_code = grpc_status_from(response.headers());
                tracker
            });

            let (parts, body) = response.into_parts();
            Ok(http::Response::from_parts(
                parts,
                MetricsGrpcBody {
                    inner: body,
                    tracker,
                },
            ))
        })
    }
}

fn grpc_status_from(headers: &http::HeaderMap) -> Option<i32> {
    headers
        .get("grpc-status")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

/// Pending measurement for one in-flight gRPC call. Consumed exactly
/// once — either at the trailer frame, at end-of-stream, or on drop.
struct GrpcRequestTracker {
    service: String,
    method: String,
    started: Instant,
    header_code: Option<i32>,
}

impl GrpcRequestTracker {
    fn record(self, code: i32) {
        counter!(GRPC_REQUESTS_TOTAL,
            LABEL_SERVICE => self.service.clone(),
            LABEL_METHOD => self.method.clone(),
            LABEL_CODE => grpc_code_label(code))
        .increment(1);
        histogram!(GRPC_REQUEST_DURATION_SECONDS,
            LABEL_SERVICE => self.service, LABEL_METHOD => self.method)
        .record(self.started.elapsed().as_secs_f64());
    }
}

pin_project_lite::pin_project! {
    /// Response-body wrapper that watches for the gRPC trailer frame.
    pub struct MetricsGrpcBody<B> {
        #[pin]
        inner: B,
        tracker: Option<GrpcRequestTracker>,
    }

    impl<B> PinnedDrop for MetricsGrpcBody<B> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            if let Some(tracker) = this.tracker.take() {
                // Body dropped before completion: the client went away.
                let code = tracker.header_code.unwrap_or(CODE_CANCELLED);
                tracker.record(code);
            }
        }
    }
}

impl<B: Body> Body for MetricsGrpcBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let result = this.inner.poll_frame(cx);

        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(trailers) = frame.trailers_ref()
                    && let Some(tracker) = this.tracker.take()
                {
                    let code = grpc_status_from(trailers)
                        .or(tracker.header_code)
                        // Trailers without grpc-status: per spec the
                        // call succeeded only if status is present;
                        // absence is unmappable → unknown (2).
                        .unwrap_or(2);
                    tracker.record(code);
                }
            }
            Poll::Ready(None) => {
                if let Some(tracker) = this.tracker.take() {
                    // Stream ended without trailers: trailers-only
                    // response, status was in the headers.
                    let code = tracker.header_code.unwrap_or(2);
                    tracker.record(code);
                }
            }
            _ => {}
        }

        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::recorder::build_recorder;
    use bytes::Bytes;
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use tower::ServiceExt;

    /// Minimal body yielding queued frames then end-of-stream.
    struct TestBody {
        frames: VecDeque<Frame<Bytes>>,
    }

    impl TestBody {
        fn with_trailers(grpc_status: &str) -> Self {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("grpc-status", grpc_status.parse().unwrap());
            Self {
                frames: VecDeque::from([
                    Frame::data(Bytes::from_static(b"payload")),
                    Frame::trailers(trailers),
                ]),
            }
        }

        fn empty() -> Self {
            Self {
                frames: VecDeque::new(),
            }
        }
    }

    impl Body for TestBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            Poll::Ready(self.frames.pop_front().map(Ok))
        }
    }

    async fn drain<B: Body + Unpin>(mut body: B) {
        while std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .is_some()
        {}
    }

    #[test]
    fn records_code_from_trailers() {
        let rendered = run_with(|| {
            http::Response::builder()
                .body(TestBody::with_trailers("0"))
                .unwrap()
        });
        assert!(
            rendered.contains(
                "guardian_grpc_requests_total{service=\"guardian.Guardian\",\
                 method=\"PushDelta\",code=\"ok\"} 1"
            ),
            "missing ok counter in:\n{rendered}"
        );
        assert!(rendered.contains("guardian_grpc_request_duration_seconds_bucket"));
    }

    #[test]
    fn records_code_from_headers_for_trailers_only_responses() {
        let rendered = run_with(|| {
            http::Response::builder()
                .header("grpc-status", "5")
                .body(TestBody::empty())
                .unwrap()
        });
        assert!(
            rendered.contains("code=\"not_found\"} 1"),
            "missing not_found counter in:\n{rendered}"
        );
    }

    #[test]
    fn dropped_body_records_cancelled() {
        let rendered = run_with_dropped_body(|| {
            http::Response::builder()
                .body(TestBody::with_trailers("0"))
                .unwrap()
        });
        assert!(
            rendered.contains("code=\"cancelled\"} 1"),
            "missing cancelled counter in:\n{rendered}"
        );
    }

    #[test]
    fn disabled_layer_records_nothing() {
        let recorder = build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let service = MetricsGrpcLayer::new(false).layer(tower::service_fn(
                    |_request: http::Request<()>| async {
                        Ok::<_, Infallible>(
                            http::Response::builder()
                                .body(TestBody::with_trailers("0"))
                                .unwrap(),
                        )
                    },
                ));
                let response = service
                    .oneshot(
                        http::Request::builder()
                            .uri("/guardian.Guardian/PushDelta")
                            .body(())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                drain(response.into_body()).await;
            });
        });
        assert!(!handle.render().contains("guardian_grpc_requests_total"));
    }

    fn run_with(make_response: fn() -> http::Response<TestBody>) -> String {
        run_case(make_response, false)
    }

    fn run_with_dropped_body(make_response: fn() -> http::Response<TestBody>) -> String {
        run_case(make_response, true)
    }

    fn run_case(make_response: fn() -> http::Response<TestBody>, drop_body_early: bool) -> String {
        let recorder = build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let service = MetricsGrpcLayer::new(true).layer(tower::service_fn(
                    move |_request: http::Request<()>| async move {
                        Ok::<_, Infallible>(make_response())
                    },
                ));
                let response = service
                    .oneshot(
                        http::Request::builder()
                            .uri("/guardian.Guardian/PushDelta")
                            .body(())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                if drop_body_early {
                    drop(response);
                } else {
                    drain(response.into_body()).await;
                }
            });
        });

        handle.render()
    }
}
