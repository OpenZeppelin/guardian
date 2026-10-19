//! The `grpc` kind: a module running as its own service.

use std::sync::Arc;

use async_trait::async_trait;
use guardian_module_sdk::proto::guardian_module_client::GuardianModuleClient;
use guardian_module_sdk::secrecy::ExposeSecret;
use guardian_module_sdk::{
    Decision, DescribeRequest, HookRequest, Module, ModuleError, ModuleFactory, ModuleInfo,
    ModuleInstanceConfig, ParamsVerdict, ValidateParamsRequest,
};
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use super::catalog::Transport;

/// Server authentication against the instance's CA, or the system roots without one; the
/// client identity, when set, adds mutual TLS.
fn tls_config(tls: Option<&guardian_module_sdk::ModuleTls>) -> ClientTlsConfig {
    let config = ClientTlsConfig::new();
    let Some(tls) = tls else {
        return config.with_native_roots();
    };
    let config = match &tls.ca_pem {
        Some(ca) => config.ca_certificate(Certificate::from_pem(ca)),
        None => config.with_native_roots(),
    };
    match &tls.client_identity {
        Some(identity) => config.identity(Identity::from_pem(
            &identity.cert_pem,
            identity.key_pem.expose_secret(),
        )),
        None => config,
    }
}

pub struct GrpcFactory;

impl ModuleFactory for GrpcFactory {
    fn create(&self, instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
        let endpoint = instance
            .endpoint
            .as_deref()
            .ok_or_else(|| ModuleError::InvalidRequest("grpc module needs an endpoint".into()))?;
        let invalid = |e: String| ModuleError::InvalidRequest(e);
        let transport = Transport::of(endpoint, instance.tls.is_some()).map_err(invalid)?;
        let endpoint = match transport {
            Transport::Tls => {
                let uri = endpoint.replacen(Transport::TLS_SCHEME, "https://", 1);
                Endpoint::from_shared(uri)
                    .map_err(|e| invalid(format!("invalid endpoint: {e}")))?
                    .tls_config(tls_config(instance.tls.as_ref()))
                    .map_err(|e| invalid(format!("invalid tls settings: {e}")))?
            }
            Transport::PlaintextLoopback => {
                let uri = endpoint.replacen(Transport::PLAINTEXT_SCHEME, "http://", 1);
                Endpoint::from_shared(uri).map_err(|e| invalid(format!("invalid endpoint: {e}")))?
            }
        };
        let channel = endpoint.connect_timeout(instance.timeout).connect_lazy();
        let authorization = instance
            .auth_token
            .as_ref()
            .map(|token| MetadataValue::try_from(format!("Bearer {}", token.expose_secret())))
            .transpose()
            .map_err(|_| ModuleError::InvalidRequest("token is not a valid header value".into()))?;
        Ok(Arc::new(GrpcModule {
            client: GuardianModuleClient::new(channel),
            instance_id: instance.id.clone(),
            authorization,
        }))
    }
}

struct GrpcModule {
    client: GuardianModuleClient<Channel>,
    instance_id: String,
    authorization: Option<MetadataValue<tonic::metadata::Ascii>>,
}

impl GrpcModule {
    fn request<T>(&self, message: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        if let Some(value) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", value.clone());
        }
        request
    }
}

fn module_error(status: tonic::Status) -> ModuleError {
    match status.code() {
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded | tonic::Code::Unknown => {
            ModuleError::Unavailable(status.message().to_string())
        }
        _ => ModuleError::Internal(format!("{}: {}", status.code(), status.message())),
    }
}

#[async_trait]
impl Module for GrpcModule {
    async fn describe(&self, guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        self.client
            .clone()
            .describe(self.request(guardian.clone()))
            .await
            .map(tonic::Response::into_inner)
            .map_err(module_error)
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        self.client
            .clone()
            .validate_params(self.request(ValidateParamsRequest {
                params: params.to_vec(),
                instance_id: self.instance_id.clone(),
            }))
            .await
            .map(tonic::Response::into_inner)
            .map_err(module_error)
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        self.client
            .clone()
            .evaluate(self.request(request.clone()))
            .await
            .map(tonic::Response::into_inner)
            .map_err(module_error)
    }
}
