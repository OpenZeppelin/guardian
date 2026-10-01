//! The execution endpoints answer the same way over HTTP and gRPC: the same code string, the
//! contract's status pair, and the same envelope and `meta`, case by case.

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use guardian_shared::auth_request_payload::AuthRequestPayload;
use tower::ServiceExt;

use crate::api::grpc::GuardianService;
use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{
    ExecuteDeltaProposalRequest, GetCurrentExecutionRequest, GetDeltaProposalExecutionRequest,
};
use crate::api::http::{CurrentExecutionQuery, ExecutionQuery};
use crate::builder::handle::{HttpRouterConfig, build_http_router};
use crate::middleware::{BodyLimitConfig, RateLimitConfig, RateLimitStore};
use crate::services::execute_proposal::ExecutionState as ExecutionServices;
use crate::services::execute_proposal::tests::{ACCOUNT, Fixture, PROPOSAL, Script};
use crate::storage::execution::ExpirationBound;
use crate::storage::{ExecutionFailure, ExecutionFailureCode};

const UNKNOWN_PROPOSAL: &str = "0x9999999999999999999999999999999999999999999999999999999999999999";

struct HttpAnswer {
    status: StatusCode,
    body: serde_json::Value,
}

struct GrpcAnswer {
    code: tonic::Code,
    body: serde_json::Value,
}

impl Fixture {
    async fn http(&self, method: &str, path: &str, payload: serde_json::Value) -> HttpAnswer {
        let signed = AuthRequestPayload::from_json_value(&payload).unwrap();
        let (pubkey, signature, timestamp) = self.sign_request(&signed);
        let (uri, body) = match method {
            "GET" => {
                let query = payload
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| format!("{key}={}", value.as_str().unwrap()))
                    .collect::<Vec<_>>()
                    .join("&");
                (format!("{path}?{query}"), Body::empty())
            }
            _ => (path.to_string(), Body::from(payload.to_string())),
        };
        let router = build_http_router(
            self.state.clone(),
            HttpRouterConfig {
                cors_layer: None,
                rate_limit_store: RateLimitStore::new(RateLimitConfig::new(10_000, 10_000)),
                body_limit_config: Some(BodyLimitConfig {
                    max_bytes: 1024 * 1024,
                }),
                metrics_enabled: false,
            },
        );
        let response = router
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-pubkey", pubkey)
                    .header("x-signature", signature)
                    .header("x-timestamp", timestamp.to_string())
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        HttpAnswer {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        }
    }

    fn grpc_request<T: prost::Message>(&self, message: T) -> tonic::Request<T> {
        let signed = AuthRequestPayload::from_protobuf_message(&message);
        let (pubkey, signature, timestamp) = self.sign_request(&signed);
        let mut request = tonic::Request::new(message);
        let metadata = request.metadata_mut();
        metadata.insert("x-pubkey", pubkey.parse().unwrap());
        metadata.insert("x-signature", signature.parse().unwrap());
        metadata.insert("x-timestamp", timestamp.to_string().parse().unwrap());
        request
    }

    async fn grpc_execute(&self, proposal_id: &str) -> GrpcAnswer {
        let service = GuardianService {
            app_state: self.state.clone(),
        };
        let request = self.grpc_request(ExecuteDeltaProposalRequest {
            account_id: ACCOUNT.to_string(),
            proposal_id: proposal_id.to_string(),
        });
        match service.execute_delta_proposal(request).await {
            Ok(response) => GrpcAnswer {
                code: tonic::Code::Ok,
                body: envelope_json(response.into_inner().execution.unwrap()),
            },
            Err(status) => refusal(status),
        }
    }

    async fn grpc_status(&self, proposal_id: &str) -> GrpcAnswer {
        let service = GuardianService {
            app_state: self.state.clone(),
        };
        let request = self.grpc_request(GetDeltaProposalExecutionRequest {
            account_id: ACCOUNT.to_string(),
            proposal_id: proposal_id.to_string(),
        });
        match service.get_delta_proposal_execution(request).await {
            Ok(response) => GrpcAnswer {
                code: tonic::Code::Ok,
                body: envelope_json(response.into_inner().execution.unwrap()),
            },
            Err(status) => refusal(status),
        }
    }

    async fn http_execute(&self, proposal_id: &str) -> HttpAnswer {
        self.http(
            "POST",
            "/delta/proposal/execution",
            serde_json::to_value(ExecutionQuery {
                account_id: ACCOUNT.to_string(),
                proposal_id: proposal_id.to_string(),
            })
            .unwrap(),
        )
        .await
    }

    async fn http_status(&self, proposal_id: &str) -> HttpAnswer {
        self.http(
            "GET",
            "/delta/proposal/execution",
            serde_json::to_value(ExecutionQuery {
                account_id: ACCOUNT.to_string(),
                proposal_id: proposal_id.to_string(),
            })
            .unwrap(),
        )
        .await
    }
}

fn refusal(status: tonic::Status) -> GrpcAnswer {
    GrpcAnswer {
        code: status.code(),
        body: serde_json::from_slice(status.details()).unwrap_or(serde_json::Value::Null),
    }
}

fn envelope_json(envelope: crate::api::grpc::guardian::ExecutionEnvelope) -> serde_json::Value {
    serde_json::json!({
        "account_id": envelope.account_id,
        "proposal_id": envelope.proposal_id,
        "state": envelope.state,
        "error": envelope.error.map(|error| serde_json::json!({
            "code": error.code,
            "meta": error.meta_json.map(|meta| serde_json::from_str::<serde_json::Value>(&meta).unwrap()),
        })),
        "newly_accepted": envelope.newly_accepted,
        "proposal_exists": envelope.proposal_exists,
        "ignored_signatures": envelope.ignored_signatures,
    })
}

/// The fields both transports carry for an envelope, in one shape.
fn http_envelope(body: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "account_id": body["account_id"],
        "proposal_id": body["proposal_id"],
        "state": body["state"],
        "error": body.get("error").map(|error| serde_json::json!({
            "code": error["code"],
            "meta": error.get("meta").cloned(),
        })),
        "newly_accepted": body["newly_accepted"],
        "proposal_exists": body["proposal_exists"],
        "ignored_signatures": body["ignored_signatures"],
    })
}

fn assert_refused_alike(
    case: &str,
    http: HttpAnswer,
    grpc: GrpcAnswer,
    code: &str,
    status: StatusCode,
    grpc_code: tonic::Code,
) {
    assert_eq!(
        http.status, status,
        "{case}: HTTP status; body {}",
        http.body
    );
    assert_eq!(
        grpc.code, grpc_code,
        "{case}: gRPC status; details {}",
        grpc.body
    );
    assert_eq!(http.body["code"], code, "{case}: HTTP code");
    assert_eq!(grpc.body["code"], code, "{case}: gRPC code");
    assert_eq!(http.body["meta"], grpc.body["meta"], "{case}: meta");
}

#[tokio::test]
async fn refusals_carry_the_same_code_and_status_pair_on_both_transports() {
    let mut unavailable = Fixture::new(Script::default()).await;
    unavailable.state.execution = ExecutionServices::default();
    assert_refused_alike(
        "no prover",
        unavailable.http_execute(PROPOSAL).await,
        unavailable.grpc_execute(PROPOSAL).await,
        "GUARDIAN_PROVING_UNAVAILABLE",
        StatusCode::SERVICE_UNAVAILABLE,
        tonic::Code::Unavailable,
    );

    let not_ready = Fixture::new(Script {
        valid: 1,
        ..Script::default()
    })
    .await;
    assert_refused_alike(
        "below threshold",
        not_ready.http_execute(PROPOSAL).await,
        not_ready.grpc_execute(PROPOSAL).await,
        "GUARDIAN_PROPOSAL_NOT_READY",
        StatusCode::CONFLICT,
        tonic::Code::FailedPrecondition,
    );

    let missing = Fixture::new(Script::default()).await;
    missing.store_proposal(false).await;
    assert_refused_alike(
        "no stored request",
        missing.http_execute(PROPOSAL).await,
        missing.grpc_execute(PROPOSAL).await,
        "GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST",
        StatusCode::CONFLICT,
        tonic::Code::FailedPrecondition,
    );

    let absent = Fixture::new(Script::default()).await;
    assert_refused_alike(
        "unknown proposal",
        absent.http_execute(UNKNOWN_PROPOSAL).await,
        absent.grpc_execute(UNKNOWN_PROPOSAL).await,
        "proposal_not_found",
        StatusCode::NOT_FOUND,
        tonic::Code::NotFound,
    );

    let never_executed = Fixture::new(Script::default()).await;
    assert_refused_alike(
        "never executed",
        never_executed.http_status(PROPOSAL).await,
        never_executed.grpc_status(PROPOSAL).await,
        "GUARDIAN_EXECUTION_NOT_FOUND",
        StatusCode::NOT_FOUND,
        tonic::Code::NotFound,
    );

    let paused = Fixture::new(Script::default()).await;
    paused
        .state
        .metadata
        .set_pause(ACCOUNT, chrono::Utc::now(), "incident")
        .await
        .unwrap();
    let (http, grpc) = (
        paused.http_execute(PROPOSAL).await,
        paused.grpc_execute(PROPOSAL).await,
    );
    assert_eq!(http.status, StatusCode::CONFLICT);
    assert_eq!(grpc.code, tonic::Code::FailedPrecondition);
    assert_eq!(http.body["code"], grpc.body["code"], "paused: code");
}

#[tokio::test]
async fn a_conflict_names_the_blocking_proposal_on_both_transports() {
    let f = Fixture::new(Script {
        submission: crate::services::execute_proposal::SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    f.settle(crate::services::execute_proposal::tests::is_submitted_or_terminal)
        .await;
    let proposal = f
        .state
        .storage
        .pull_delta_proposal(ACCOUNT, PROPOSAL)
        .await
        .unwrap();
    f.state
        .storage
        .submit_delta_proposal(UNKNOWN_PROPOSAL, &proposal)
        .await
        .unwrap();

    let (http, grpc) = (
        f.http_execute(UNKNOWN_PROPOSAL).await,
        f.grpc_execute(UNKNOWN_PROPOSAL).await,
    );
    assert_refused_alike(
        "conflict",
        http,
        grpc,
        "GUARDIAN_EXECUTION_CONFLICT",
        StatusCode::CONFLICT,
        tonic::Code::Aborted,
    );
}

#[tokio::test]
async fn a_lease_held_without_a_reservation_is_busy_and_retryable_on_both_transports() {
    let f = Fixture::new(Script::default()).await;
    let _held = f
        .state
        .execution
        .leases
        .elector(ACCOUNT, "another-request")
        .try_acquire(std::time::Duration::from_secs(60))
        .await
        .unwrap()
        .expect("the lease is free");

    let (http, grpc) = (
        f.http_execute(PROPOSAL).await,
        f.grpc_execute(PROPOSAL).await,
    );
    assert_eq!(http.body["meta"]["retryable"], true, "{}", http.body);
    assert!(http.body["meta"].get("blocking_proposal_id").is_none());
    assert_refused_alike(
        "busy",
        http,
        grpc,
        "GUARDIAN_EXECUTION_BUSY",
        StatusCode::CONFLICT,
        tonic::Code::Aborted,
    );
}

#[tokio::test]
async fn acceptance_and_repetition_are_carried_by_newly_accepted_on_both_transports() {
    let over_http = Fixture::new(Script {
        submission: crate::services::execute_proposal::SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    let accepted = over_http.http_execute(PROPOSAL).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    assert_eq!(accepted.body["newly_accepted"], true);
    let repeated = over_http.http_execute(PROPOSAL).await;
    assert_eq!(repeated.status, StatusCode::OK);
    assert_eq!(repeated.body["newly_accepted"], false);

    let over_grpc = Fixture::new(Script {
        submission: crate::services::execute_proposal::SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    let accepted = over_grpc.grpc_execute(PROPOSAL).await;
    assert_eq!(accepted.code, tonic::Code::Ok);
    assert_eq!(accepted.body["newly_accepted"], true);
    assert_eq!(accepted.body["state"], "pending");
    assert_eq!(accepted.body["ignored_signatures"], 1);
    let repeated = over_grpc.grpc_execute(PROPOSAL).await;
    assert_eq!(repeated.body["newly_accepted"], false);
}

#[tokio::test]
async fn a_failed_execution_reads_the_same_with_its_meta_on_both_transports() {
    let f = Fixture::new(Script {
        prepare: Err(ExecutionFailure {
            code: ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval),
            message: "expired".to_string(),
        }),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    f.settle(|state| state == crate::services::execution_status::ExecutionState::Failed)
        .await;

    let http = f.http_status(PROPOSAL).await;
    let grpc = f.grpc_status(PROPOSAL).await;
    assert_eq!(http.status, StatusCode::OK);
    assert_eq!(grpc.code, tonic::Code::Ok);
    assert_eq!(http_envelope(&http.body), grpc.body);
    assert_eq!(grpc.body["state"], "failed");
    assert_eq!(
        grpc.body["error"]["meta"],
        serde_json::json!({ "bound": "approval" })
    );
    assert_eq!(grpc.body["proposal_exists"], true);
}

#[tokio::test]
async fn nothing_in_flight_is_a_success_on_both_transports() {
    let f = Fixture::new(Script::default()).await;
    let http = f
        .http(
            "GET",
            "/delta/execution/current",
            serde_json::to_value(CurrentExecutionQuery {
                account_id: ACCOUNT.to_string(),
            })
            .unwrap(),
        )
        .await;
    assert_eq!(http.status, StatusCode::OK);
    assert_eq!(http.body, serde_json::json!({ "execution": null }));

    let service = GuardianService {
        app_state: f.state.clone(),
    };
    let response = service
        .get_current_execution(f.grpc_request(GetCurrentExecutionRequest {
            account_id: ACCOUNT.to_string(),
        }))
        .await
        .unwrap();
    assert!(response.into_inner().execution.is_none());
}
