//! gRPC harness exposing any [`Module`] as a `guardian.module.v1.GuardianModule` service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::proto::NotifyAck;
use crate::proto::guardian_module_server::{GuardianModule, GuardianModuleServer};
use crate::{
    CommitEvent, Decision, DescribeRequest, Gap, HookRequest, Module, ModuleError, ModuleInfo,
    ParamsVerdict, ValidateParamsRequest,
};

pub fn serve<M: Module + 'static>(module: M) -> Serve<M> {
    Serve {
        module: Arc::new(module),
        tokens: HashMap::new(),
    }
}

pub struct Serve<M> {
    module: Arc<M>,
    tokens: HashMap<String, String>,
}

impl<M: Module + 'static> Serve<M> {
    /// Accept requests for `instance_id` only when they carry `token` as a bearer credential.
    /// With no instance registered, every request is accepted.
    pub fn instance(mut self, instance_id: impl Into<String>, token: impl Into<String>) -> Self {
        self.tokens.insert(instance_id.into(), token.into());
        self
    }

    pub fn into_service(self) -> GuardianModuleServer<Harness<M>> {
        GuardianModuleServer::new(Harness {
            module: self.module,
            tokens: Arc::new(self.tokens),
        })
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
    tokens: Arc<HashMap<String, String>>,
}

impl<M> Harness<M> {
    fn authorize<T>(&self, request: &Request<T>, instance_id: Option<&str>) -> Result<(), Status> {
        if self.tokens.is_empty() {
            return Ok(());
        }
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let accepted = match instance_id {
            Some(id) => self.tokens.get(id).is_some_and(|token| token == presented),
            None => self.tokens.values().any(|token| token == presented),
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

fn status(error: ModuleError) -> Status {
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
            .describe()
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

    async fn notify(&self, request: Request<CommitEvent>) -> Result<Response<NotifyAck>, Status> {
        self.authorize(&request, Some(&request.get_ref().instance_id))?;
        self.module
            .notify(request.get_ref())
            .await
            .map(|()| Response::new(NotifyAck {}))
            .map_err(status)
    }

    async fn notify_gap(&self, request: Request<Gap>) -> Result<Response<NotifyAck>, Status> {
        self.authorize(&request, Some(&request.get_ref().instance_id))?;
        self.module
            .notify_gap(request.get_ref())
            .await
            .map(|()| Response::new(NotifyAck {}))
            .map_err(status)
    }
}
