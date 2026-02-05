use crate::proto::state_manager_server::{StateManager, StateManagerServer};
use crate::proto::{
    AccountState, ConfigureRequest, ConfigureResponse, DeltaObject as ProtoDeltaObject,
    GetDeltaProposalsRequest, GetDeltaProposalsResponse, GetDeltaRequest, GetDeltaResponse,
    GetDeltaSinceRequest, GetDeltaSinceResponse, GetPubkeyRequest, GetStateRequest,
    GetStateResponse, PushDeltaProposalRequest, PushDeltaProposalResponse, PushDeltaRequest,
    PushDeltaResponse, SignDeltaProposalRequest, SignDeltaProposalResponse,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use tonic::transport::Server;
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct MockStateManagerService {
    get_pubkey_response: Arc<StdMutex<Option<Result<String, Status>>>>,
    configure_response: Arc<StdMutex<Option<Result<ConfigureResponse, Status>>>>,
    push_delta_proposal_response: Arc<StdMutex<Option<Result<PushDeltaProposalResponse, Status>>>>,
    get_delta_proposals_response: Arc<StdMutex<Option<Result<GetDeltaProposalsResponse, Status>>>>,
    sign_delta_proposal_response: Arc<StdMutex<Option<Result<SignDeltaProposalResponse, Status>>>>,
    push_delta_response: Arc<StdMutex<Option<Result<PushDeltaResponse, Status>>>>,
    get_delta_response: Arc<StdMutex<Option<Result<GetDeltaResponse, Status>>>>,
    get_delta_since_response: Arc<StdMutex<Option<Result<GetDeltaSinceResponse, Status>>>>,
    get_state_response: Arc<StdMutex<Option<Result<GetStateResponse, Status>>>>,
}

impl MockStateManagerService {
    pub fn with_get_pubkey(self, response: Result<String, Status>) -> Self {
        *self.get_pubkey_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_configure(self, response: Result<ConfigureResponse, Status>) -> Self {
        *self.configure_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_push_delta_proposal(
        self,
        response: Result<PushDeltaProposalResponse, Status>,
    ) -> Self {
        *self.push_delta_proposal_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_get_delta_proposals(
        self,
        response: Result<GetDeltaProposalsResponse, Status>,
    ) -> Self {
        *self.get_delta_proposals_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_sign_delta_proposal(
        self,
        response: Result<SignDeltaProposalResponse, Status>,
    ) -> Self {
        *self.sign_delta_proposal_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_push_delta(self, response: Result<PushDeltaResponse, Status>) -> Self {
        *self.push_delta_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_get_delta(self, response: Result<GetDeltaResponse, Status>) -> Self {
        *self.get_delta_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_get_delta_since(self, response: Result<GetDeltaSinceResponse, Status>) -> Self {
        *self.get_delta_since_response.lock().unwrap() = Some(response);
        self
    }

    pub fn with_get_state(self, response: Result<GetStateResponse, Status>) -> Self {
        *self.get_state_response.lock().unwrap() = Some(response);
        self
    }
}

#[tonic::async_trait]
impl StateManager for MockStateManagerService {
    async fn get_pubkey(
        &self,
        _request: Request<GetPubkeyRequest>,
    ) -> Result<Response<crate::proto::GetPubkeyResponse>, Status> {
        let response = self
            .get_pubkey_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Ok("mock_pubkey".to_string()));

        response.map(|pubkey| {
            Response::new(crate::proto::GetPubkeyResponse {
                pubkey,
                raw_pubkey: None,
            })
        })
    }

    async fn configure(
        &self,
        _request: Request<ConfigureRequest>,
    ) -> Result<Response<ConfigureResponse>, Status> {
        let response = self
            .configure_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(ConfigureResponse {
                    success: true,
                    message: String::new(),
                    ack_pubkey: "mock_ack_pubkey".to_string(),
                    ack_commitment: String::new(),
                })
            });

        response.map(Response::new)
    }

    async fn push_delta_proposal(
        &self,
        _request: Request<PushDeltaProposalRequest>,
    ) -> Result<Response<PushDeltaProposalResponse>, Status> {
        let response = self
            .push_delta_proposal_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(PushDeltaProposalResponse {
                    success: true,
                    message: String::new(),
                    commitment: "mock_commitment".to_string(),
                    delta: Some(create_mock_delta()),
                })
            });

        response.map(Response::new)
    }

    async fn get_delta_proposals(
        &self,
        _request: Request<GetDeltaProposalsRequest>,
    ) -> Result<Response<GetDeltaProposalsResponse>, Status> {
        let response = self
            .get_delta_proposals_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(GetDeltaProposalsResponse {
                    success: true,
                    message: String::new(),
                    proposals: vec![],
                })
            });

        response.map(Response::new)
    }

    async fn sign_delta_proposal(
        &self,
        _request: Request<SignDeltaProposalRequest>,
    ) -> Result<Response<SignDeltaProposalResponse>, Status> {
        let response = self
            .sign_delta_proposal_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(SignDeltaProposalResponse {
                    success: true,
                    message: String::new(),
                    delta: Some(create_mock_delta()),
                })
            });

        response.map(Response::new)
    }

    async fn push_delta(
        &self,
        _request: Request<PushDeltaRequest>,
    ) -> Result<Response<PushDeltaResponse>, Status> {
        let response = self
            .push_delta_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(PushDeltaResponse {
                    success: true,
                    message: String::new(),
                    delta: Some(create_mock_delta()),
                    ack_sig: None,
                })
            });

        response.map(Response::new)
    }

    async fn get_delta(
        &self,
        _request: Request<GetDeltaRequest>,
    ) -> Result<Response<GetDeltaResponse>, Status> {
        let response = self
            .get_delta_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(GetDeltaResponse {
                    success: true,
                    message: String::new(),
                    delta: Some(create_mock_delta()),
                })
            });

        response.map(Response::new)
    }

    async fn get_delta_since(
        &self,
        _request: Request<GetDeltaSinceRequest>,
    ) -> Result<Response<GetDeltaSinceResponse>, Status> {
        let response = self
            .get_delta_since_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(GetDeltaSinceResponse {
                    success: true,
                    message: String::new(),
                    merged_delta: Some(create_mock_delta()),
                })
            });

        response.map(Response::new)
    }

    async fn get_state(
        &self,
        _request: Request<GetStateRequest>,
    ) -> Result<Response<GetStateResponse>, Status> {
        let response = self
            .get_state_response
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(GetStateResponse {
                    success: true,
                    message: String::new(),
                    state: Some(create_mock_account_state()),
                })
            });

        response.map(Response::new)
    }
}

pub async fn start_mock_server(
    service: MockStateManagerService,
) -> Result<String, Box<dyn std::error::Error>> {
    let addr: SocketAddr = "127.0.0.1:0".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    let endpoint = format!("http://{}", local_addr);

    tokio::spawn(async move {
        Server::builder()
            .add_service(StateManagerServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    Ok(endpoint)
}

pub fn create_mock_delta() -> ProtoDeltaObject {
    ProtoDeltaObject {
        account_id: "0x7bfb0f38b0fafa103f86a805594170".to_string(),
        nonce: 1,
        prev_commitment: "0x123".to_string(),
        delta_payload: r#"{"updates": []}"#.to_string(),
        new_commitment: "0x456".to_string(),
        ack_sig: String::new(),
        candidate_at: String::new(),
        canonical_at: None,
        discarded_at: None,
        status: None,
        ack_pubkey: None,
        ack_scheme: None,
    }
}

pub fn create_mock_account_state() -> AccountState {
    AccountState {
        account_id: "0x7bfb0f38b0fafa103f86a805594170".to_string(),
        state_json: r#"{"balance": 1000}"#.to_string(),
        commitment: "0x123".to_string(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        updated_at: "2024-01-01T00:00:00Z".to_string(),
    }
}
