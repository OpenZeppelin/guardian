//! Reclassifies tonic's own `INTERNAL` response for a request without a
//! message body as `INVALID_ARGUMENT`. Tonic rejects such requests before any
//! handler runs, so the status never passes through `GuardianError`, and as
//! `INTERNAL` it would count toward the server-fault alarm.

use futures::TryFutureExt;
use futures::future::MapOk;
use std::task::{Context, Poll};
use tower::{Layer, Service};

const GRPC_STATUS: &str = "grpc-status";
const INTERNAL: &str = "13";
const TONIC_MISSING_MESSAGE: &str = "Missing request message.";

#[derive(Debug, Clone, Default)]
pub struct MissingMessageStatusLayer;

impl<S> Layer<S> for MissingMessageStatusLayer {
    type Service = MissingMessageStatusService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MissingMessageStatusService { inner }
    }
}

#[derive(Debug, Clone)]
pub struct MissingMessageStatusService<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<http::Request<ReqBody>> for MissingMessageStatusService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    ResBody: Default,
{
    type Response = http::Response<ResBody>;
    type Error = S::Error;
    type Future = MapOk<S::Future, fn(http::Response<ResBody>) -> http::Response<ResBody>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<ReqBody>) -> Self::Future {
        self.inner.call(request).map_ok(reclassify_missing_message)
    }
}

fn reclassify_missing_message<B: Default>(response: http::Response<B>) -> http::Response<B> {
    if is_missing_message(response.headers()) {
        tonic::Status::invalid_argument("Request body must contain a message").into_http()
    } else {
        response
    }
}

fn is_missing_message(headers: &http::HeaderMap) -> bool {
    headers
        .get(GRPC_STATUS)
        .is_some_and(|status| status == INTERNAL)
        && tonic::Status::from_header_map(headers)
            .is_some_and(|status| status.message() == TONIC_MISSING_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(response: &http::Response<tonic::body::Body>) -> tonic::Status {
        tonic::Status::from_header_map(response.headers()).expect("status in headers")
    }

    #[test]
    fn tonic_missing_message_becomes_invalid_argument() {
        let response = reclassify_missing_message(
            tonic::Status::internal(TONIC_MISSING_MESSAGE).into_http::<tonic::body::Body>(),
        );
        assert_eq!(status_of(&response).code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn other_internal_errors_are_untouched() {
        let response = reclassify_missing_message(
            tonic::Status::internal("database unavailable").into_http::<tonic::body::Body>(),
        );
        let status = status_of(&response);
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "database unavailable");
    }

    #[test]
    fn successful_responses_are_untouched() {
        let response =
            reclassify_missing_message(http::Response::new(tonic::body::Body::default()));
        assert!(response.headers().get(GRPC_STATUS).is_none());
    }
}
