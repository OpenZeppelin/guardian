//! gRPC harness exposing any [`Module`] as a `guardian.module.v1.GuardianModule` service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};

use crate::proto::guardian_module_server::{GuardianModule, GuardianModuleServer};
use crate::{
    Decision, DescribeRequest, HookRequest, Module, ModuleError, ModuleInfo, ParamsVerdict,
    ValidateParamsRequest,
};

/// Serves `module`. Register every instance the Guardian calls with [`Serve::instance`]: a
/// harness with none refuses every call, unless [`Serve::allow_unauthenticated`] says otherwise.
/// Register an instance once per token it accepts: during a token rotation, register the old and
/// the new token until every Guardian sends the new one.
pub fn serve<M: Module + 'static>(module: M) -> Serve<M> {
    Serve {
        module: Arc::new(module),
        tokens: HashMap::new(),
        access: Access::Instances,
        request_max_bytes: None,
    }
}

/// Who the harness answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    /// Calls for a registered instance, carrying that instance's token.
    Instances,
    /// Every call, without credentials: for tests and local experiments only.
    Anyone,
}

pub struct Serve<M> {
    module: Arc<M>,
    tokens: HashMap<String, Vec<TokenDigest>>,
    access: Access,
    request_max_bytes: Option<usize>,
}

impl<M: Module + 'static> Serve<M> {
    /// Accept requests for `instance_id` when they carry `token` as a bearer credential.
    /// Registering the same instance again adds a token it also accepts. An empty token is
    /// accepted from no one. Only the token's digest is kept.
    pub fn instance(mut self, instance_id: impl Into<String>, token: impl Into<String>) -> Self {
        let token = token.into();
        let tokens = self.tokens.entry(instance_id.into()).or_default();
        if !token.is_empty() {
            tokens.push(TokenDigest::of(&token));
        }
        self
    }

    /// Whether any instance is registered.
    pub fn has_instances(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// Answers every call without checking credentials, registered instances or not. Anyone
    /// who can reach the address can then query the module and read its params; use it only
    /// for tests and local experiments on a loopback address.
    pub fn allow_unauthenticated(mut self) -> Self {
        self.access = Access::Anyone;
        self
    }

    /// Accepts requests up to `bytes` encoded bytes. Set it to the Guardian's
    /// `GUARDIAN_MODULE_REQUEST_MAX_BYTES` when that is above gRPC's default receive limit of
    /// 4 MiB, or the harness refuses the largest requests the Guardian sends.
    pub fn request_max_bytes(mut self, bytes: usize) -> Self {
        self.request_max_bytes = Some(bytes);
        self
    }

    pub fn into_service(self) -> GuardianModuleServer<Harness<M>> {
        let server = GuardianModuleServer::new(Harness {
            module: self.module,
            tokens: Arc::new(self.tokens),
            access: self.access,
        });
        match self.request_max_bytes {
            Some(bytes) => server.max_decoding_message_size(bytes),
            None => server,
        }
    }

    pub async fn run(self, addr: SocketAddr) -> Result<(), tonic::transport::Error> {
        tonic::transport::Server::builder()
            .add_service(self.into_service())
            .serve(addr)
            .await
    }
}

pub struct Harness<M> {
    module: Arc<M>,
    tokens: Arc<HashMap<String, Vec<TokenDigest>>>,
    access: Access,
}

impl<M> Harness<M> {
    fn authorize<T>(&self, request: &Request<T>, instance_id: Option<&str>) -> Result<(), Status> {
        match (self.access, self.tokens.is_empty()) {
            (Access::Anyone, _) => return Ok(()),
            (Access::Instances, true) => {
                return Err(Status::unauthenticated(
                    "no instance is registered with this module server",
                ));
            }
            (Access::Instances, false) => {}
        }
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let presented = TokenDigest::of(presented);
        let any_matches = |tokens: &mut dyn Iterator<Item = &TokenDigest>| {
            tokens.fold(false, |matched, token| token.matches(&presented) | matched)
        };
        let accepted = match instance_id {
            Some(id) => self
                .tokens
                .get(id)
                .is_some_and(|tokens| any_matches(&mut tokens.iter())),
            None => any_matches(&mut self.tokens.values().flatten()),
        };
        if accepted {
            Ok(())
        } else {
            Err(Status::permission_denied(
                "credential does not match the instance",
            ))
        }
    }
}

/// A bearer token as its SHA-256 digest. Tokens are compared as digests, in constant time, so the
/// work does not depend on where two tokens differ or on how long the registered one is, and the
/// harness keeps no copy of the token itself.
#[derive(Clone)]
struct TokenDigest([u8; 32]);

impl TokenDigest {
    fn of(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }

    fn matches(&self, presented: &TokenDigest) -> bool {
        self.0.ct_eq(&presented.0).into()
    }
}

pub(crate) fn status(error: ModuleError) -> Status {
    match error {
        ModuleError::Unavailable(message) => Status::unavailable(message),
        ModuleError::InvalidRequest(message) => Status::invalid_argument(message),
        ModuleError::Internal(message) => Status::internal(message),
    }
}

#[tonic::async_trait]
impl<M: Module + 'static> GuardianModule for Harness<M> {
    async fn describe(
        &self,
        request: Request<DescribeRequest>,
    ) -> Result<Response<ModuleInfo>, Status> {
        self.authorize(&request, None)?;
        self.module
            .describe(request.get_ref())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn validate_params(
        &self,
        request: Request<ValidateParamsRequest>,
    ) -> Result<Response<ParamsVerdict>, Status> {
        self.authorize(&request, Some(&request.get_ref().instance_id))?;
        self.module
            .validate_params(&request.get_ref().params)
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn evaluate(&self, request: Request<HookRequest>) -> Result<Response<Decision>, Status> {
        self.authorize(&request, Some(&request.get_ref().instance_id))?;
        self.module
            .evaluate(request.get_ref())
            .await
            .map(Response::new)
            .map_err(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness(
        server: Serve<crate::policies::SpendingLimit>,
    ) -> Harness<crate::policies::SpendingLimit> {
        Harness {
            module: server.module,
            tokens: Arc::new(server.tokens),
            access: server.access,
        }
    }

    #[test]
    fn a_server_without_instances_answers_nobody_unless_told_to() {
        let unregistered = harness(serve(crate::policies::SpendingLimit));
        assert_eq!(
            unregistered
                .authorize(&Request::new(()), Some("caps"))
                .unwrap_err()
                .code(),
            tonic::Code::Unauthenticated
        );
        let open = harness(serve(crate::policies::SpendingLimit).allow_unauthenticated());
        assert!(open.authorize(&Request::new(()), Some("caps")).is_ok());
    }

    fn bearer(token: &str) -> Request<()> {
        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("ascii token"),
        );
        request
    }

    #[test]
    fn an_instance_registered_twice_accepts_both_tokens_during_a_rotation() {
        let rotating = harness(
            serve(crate::policies::SpendingLimit)
                .instance("caps", "old-token")
                .instance("caps", "new-token")
                .instance("other", "other-token"),
        );
        for token in ["old-token", "new-token"] {
            assert!(rotating.authorize(&bearer(token), Some("caps")).is_ok());
            assert!(rotating.authorize(&bearer(token), None).is_ok());
        }
        for (token, instance) in [("other-token", "caps"), ("old-token", "other")] {
            assert_eq!(
                rotating
                    .authorize(&bearer(token), Some(instance))
                    .unwrap_err()
                    .code(),
                tonic::Code::PermissionDenied
            );
        }
    }

    #[test]
    fn only_the_registered_token_matches() {
        let registered = TokenDigest::of("secret-token");
        assert!(registered.matches(&TokenDigest::of("secret-token")));
        for presented in ["", "secret", "secret-tokeN", "secret-token-and-more", "x"] {
            assert!(
                !registered.matches(&TokenDigest::of(presented)),
                "{presented:?}"
            );
        }
        let empty = harness(serve(crate::policies::SpendingLimit).instance("caps", ""));
        assert_eq!(
            empty
                .authorize(&bearer(""), Some("caps"))
                .map_err(|status| status.code()),
            Err(tonic::Code::PermissionDenied),
            "an empty registered token matches nothing"
        );
    }
}
