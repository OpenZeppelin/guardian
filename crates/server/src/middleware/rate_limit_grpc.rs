//! Rate limiting for the tonic gRPC server.
//!
//! A tower layer sharing one [`RateLimitStore`] with the HTTP side, so
//! both transports draw from a single sustained budget. Keying is
//! identical to HTTP (gRPC metadata are HTTP/2 headers), with the
//! normalized proto method as the per-endpoint burst key: burst buckets
//! are per-path by design, so HTTP `/delta` and gRPC
//! `/guardian.Guardian/PushDelta` are intentionally separate burst
//! buckets, and unserved paths collapse into one `unknown/unknown`
//! bucket so path-spraying cannot mint unbounded entries. Rejections
//! short-circuit before the service handler with `ResourceExhausted`, a
//! `retry-after` metadata hint, and the canonical error envelope in the
//! status details.

use futures::future::Either;
use std::future::{Ready, ready};
use std::task::{Context, Poll};
use tower::{Layer, Service};

use super::rate_limit::RateLimitStore;
use crate::metrics::names::{TRANSPORT_GRPC, normalize_grpc_method};

#[derive(Debug, Clone)]
pub struct GrpcRateLimitLayer {
    store: RateLimitStore,
}

impl GrpcRateLimitLayer {
    pub fn new(store: RateLimitStore) -> Self {
        Self { store }
    }
}

impl<S> Layer<S> for GrpcRateLimitLayer {
    type Service = GrpcRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcRateLimitService {
            inner,
            store: self.store.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrpcRateLimitService<S> {
    inner: S,
    store: RateLimitStore,
}

impl<S, ReqBody, ResBody> Service<http::Request<ReqBody>> for GrpcRateLimitService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    ResBody: Default,
{
    type Response = http::Response<ResBody>;
    type Error = S::Error;
    type Future = Either<S::Future, Ready<Result<Self::Response, S::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let (service, method) = normalize_grpc_method(req.uri().path());
        match self
            .store
            .check_request(&req, &format!("{service}/{method}"))
        {
            Ok(()) => Either::Left(self.inner.call(req)),
            Err(rejection) => {
                let status: tonic::Status = rejection.into_error(TRANSPORT_GRPC).into();
                Either::Right(ready(Ok(status.into_http())))
            }
        }
    }
}
