use guardian_module_sdk::basic_policy::BasicPolicy;
use guardian_module_sdk::proto::guardian_module_client::GuardianModuleClient;
use guardian_module_sdk::proto::{asset, hook_request};
use guardian_module_sdk::serve::serve;
use guardian_module_sdk::{
    AccountView, Asset, HookRequest, MidenTransaction, Module, OutputNote, TransactionProjection,
    TransactionSubject,
};
use serde_json::json;
use tokio::net::TcpListener;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const INSTANCE: &str = "caps";
const TOKEN: &str = "secret-token";

async fn start_remote() -> Channel {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = serve(BasicPolicy).instance(INSTANCE, TOKEN).into_service();
    tokio::spawn(async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpIncoming::from(listener))
            .await
            .unwrap();
    });
    Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

fn authorized<T>(message: T, token: &str) -> Request<T> {
    let mut request = Request::new(message);
    request.metadata_mut().insert(
        "authorization",
        MetadataValue::try_from(format!("Bearer {token}")).unwrap(),
    );
    request
}

fn payment(amounts: &[u64], params: serde_json::Value) -> HookRequest {
    let notes = amounts
        .iter()
        .enumerate()
        .map(|(i, amount)| OutputNote {
            note_id: format!("0x{i}"),
            assets: vec![Asset {
                faucet_id: "0xaa".into(),
                value: Some(asset::Value::FungibleAmount(amount.to_string())),
            }],
            p2id_target: Some("0xr1".into()),
            ..Default::default()
        })
        .collect();
    HookRequest {
        instance_id: INSTANCE.into(),
        params: serde_json::to_vec(&params).unwrap(),
        after: Some(AccountView::default()),
        subject: Some(hook_request::Subject::Transaction(TransactionSubject {
            projection: Some(TransactionProjection::miden(MidenTransaction {
                output_notes: notes,
                ..Default::default()
            })),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[tokio::test]
async fn in_process_and_remote_decisions_match() {
    let mut client = GuardianModuleClient::new(start_remote().await);
    let cap = json!({ "max_amount_per_tx": { "0xaa": "1000" } });
    let cases = [
        payment(&[500], cap.clone()),
        payment(&[600, 600], cap.clone()),
        payment(&[1000], cap.clone()),
        payment(&[1], json!({ "recipient_allowlist": ["0xother"] })),
    ];
    for case in cases {
        let local = BasicPolicy.evaluate(&case).await.unwrap();
        let remote = client
            .evaluate(authorized(case.clone(), TOKEN))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(local, remote);
        let again = client
            .evaluate(authorized(case, TOKEN))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(remote, again);
    }
}

#[tokio::test]
async fn remote_rejects_a_credential_for_another_instance() {
    let mut client = GuardianModuleClient::new(start_remote().await);
    let mut request = payment(&[1], json!({}));
    request.instance_id = "other-instance".into();
    let status = client
        .evaluate(authorized(request, TOKEN))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);

    let status = client
        .evaluate(authorized(payment(&[1], json!({})), "wrong"))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
#[ignore = "timing report for the prototype gates; run with --ignored --nocapture"]
async fn loopback_evaluate_round_trip() {
    let mut client = GuardianModuleClient::new(start_remote().await);
    let request = payment(&[500], json!({ "max_amount_per_tx": { "0xaa": "1000" } }));
    for _ in 0..50 {
        client
            .evaluate(authorized(request.clone(), TOKEN))
            .await
            .unwrap();
    }
    let mut samples = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let started = std::time::Instant::now();
        client
            .evaluate(authorized(request.clone(), TOKEN))
            .await
            .unwrap();
        samples.push(started.elapsed());
    }
    samples.sort();
    let local_started = std::time::Instant::now();
    for _ in 0..1000 {
        BasicPolicy.evaluate(&request).await.unwrap();
    }
    println!(
        "remote p50 {:?} p99 {:?}; in-process mean {:?}",
        samples[500],
        samples[990],
        local_started.elapsed() / 1000
    );
}
