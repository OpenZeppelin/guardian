#[path = "../examples/note_policy/mod.rs"]
mod note_policy;

use guardian_module_sdk::conformance::Suite;
use guardian_module_sdk::policies::{CodePolicy, RecipientPolicy, SignerPolicy, SpendingLimit};
use guardian_module_sdk::proto::guardian_module_client::GuardianModuleClient;
use guardian_module_sdk::proto::{asset, hook_request};
use guardian_module_sdk::serve::serve;
use guardian_module_sdk::{
    AccountStateProjection, AccountView, Asset, HookRequest, MidenAccountState, MidenTransaction,
    Module, NoteType, OutputNote, RegistrationSubject, TransactionProjection, TransactionSubject,
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
    let service = serve(SpendingLimit)
        .instance(INSTANCE, TOKEN)
        .into_service();
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
                faucet_id: "0x0000000000000000000000000000aa".into(),
                value: Some(asset::Value::FungibleAmount(amount.to_string())),
            }],
            p2id_target: Some("0x0000000000000000000000000000a1".into()),
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

fn view(signers: &[&str], code: &str) -> AccountView {
    AccountView {
        signer_commitments: signers.iter().map(|s| s.to_string()).collect(),
        code_commitment: code.into(),
        ..Default::default()
    }
}

fn with_views(mut request: HookRequest, before: &[&str], after: &[&str]) -> HookRequest {
    request.before = Some(view(
        before,
        "0x00000000000000000000000000000000000000000000000000000000000000c1",
    ));
    request.after = Some(view(
        after,
        "0x00000000000000000000000000000000000000000000000000000000000000c1",
    ));
    request
}

fn registration(params: serde_json::Value, signers: &[&str]) -> HookRequest {
    HookRequest {
        params: serde_json::to_vec(&params).unwrap(),
        after: Some(view(
            signers,
            "0x00000000000000000000000000000000000000000000000000000000000000c1",
        )),
        subject: Some(hook_request::Subject::Registration(RegistrationSubject {
            initial_state: Some(AccountStateProjection::miden(MidenAccountState::default())),
        })),
        ..Default::default()
    }
}

const FAUCET: &str = "0x0000000000000000000000000000aa";
const SIGNER: &str = "0x0000000000000000000000000000000000000000000000000000000000000051";
const BAD_SIGNER: &str = "0x0000000000000000000000000000000000000000000000000000000000000bad";
const CODE: &str = "0x00000000000000000000000000000000000000000000000000000000000000c1";

#[tokio::test]
async fn spending_limit_conforms() {
    let cap = json!({ "max_per_transaction": { FAUCET: "1000" } });
    Suite::new(SpendingLimit)
        .allows("under the cap", payment(&[500], cap.clone()))
        .denies_with(
            "split over the cap",
            payment(&[600, 600], cap.clone()),
            "amount_cap_exceeded",
        )
        .allows("at the cap", payment(&[1000], cap))
        .denies_with(
            "an asset that is not allowed",
            payment(
                &[1],
                json!({ "allowed_assets": ["0x0000000000000000000000000000bb"] }),
            ),
            "asset_not_allowed",
        )
        .fails(
            "params a module cannot read",
            payment(&[1], json!({ "max_per_transaction": { FAUCET: "ten" } })),
        )
        .valid_params(json!({}))
        .valid_params(
            json!({ "max_per_transaction": { FAUCET: "1000" }, "allowed_assets": [FAUCET] }),
        )
        .invalid_params(json!({ "unknown": 1 }))
        .invalid_params(json!({ "max_per_transaction": { FAUCET: "ten" } }))
        .invalid_params(json!({ "denylist": [] }))
        .run()
        .await
        .assert_conformant();
}

#[tokio::test]
async fn recipient_policy_conforms() {
    Suite::new(RecipientPolicy)
        .allows(
            "a listed recipient",
            payment(
                &[1],
                json!({ "allowlist": ["0x0000000000000000000000000000a1"] }),
            ),
        )
        .denies_with(
            "a recipient not allowed",
            payment(
                &[1],
                json!({ "allowlist": ["0x0000000000000000000000000007e4"] }),
            ),
            "recipient_not_allowed",
        )
        .denies_with(
            "a blocked recipient",
            payment(
                &[1],
                json!({ "denylist": ["0x0000000000000000000000000000a1"] }),
            ),
            "recipient_blocked",
        )
        .valid_params(json!({ "denylist": [] }))
        .valid_params(
            json!({ "denylist": ["0x0000000000000000000000000000a1"], "deny_unknown": false }),
        )
        .invalid_params(json!({ "allowlist": [], "denylist": [] }))
        .invalid_params(json!({ "deny_unknown": true }))
        .run()
        .await
        .assert_conformant();
}

#[tokio::test]
async fn signer_policy_conforms() {
    let signers = json!({ "check": "added", "denylist": [BAD_SIGNER] });
    Suite::new(SignerPolicy)
        .denies_with(
            "a denied signer added",
            with_views(
                payment(&[1], signers.clone()),
                &[SIGNER],
                &[SIGNER, BAD_SIGNER],
            ),
            "signer_not_allowed",
        )
        .allows(
            "no signer added",
            with_views(payment(&[1], signers.clone()), &[SIGNER], &[SIGNER]),
        )
        .denies(
            "registration with a denied signer",
            registration(signers, &[BAD_SIGNER]),
        )
        .valid_params(json!({ "allowlist": [SIGNER] }))
        .invalid_params(json!({ "allowlist": [], "denylist": [] }))
        .invalid_params(json!({ "check": "all" }))
        .run()
        .await
        .assert_conformant();
}

#[tokio::test]
async fn code_policy_conforms() {
    let code = json!({ "check": "all", "allowed": [CODE] });
    Suite::new(CodePolicy)
        .allows(
            "listed code",
            with_views(payment(&[1], code.clone()), &[SIGNER], &[SIGNER]),
        )
        .allows("registration on listed code", registration(code, &[SIGNER]))
        .denies_with(
            "registration on code that is not listed",
            registration(
                json!({ "allowed": ["0x00000000000000000000000000000000000000000000000000000000000000c2"] }),
                &[SIGNER],
            ),
            "code_not_allowed",
        )
        .valid_params(json!({ "check": "changed", "allowed": [CODE] }))
        .invalid_params(json!({ "check": "all" }))
        .invalid_params(json!({ "allowed": ["0xc1"] }))
        .run()
        .await
        .assert_conformant();
}

fn notes(notes: Vec<OutputNote>, params: serde_json::Value) -> HookRequest {
    HookRequest {
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

fn note(note_type: NoteType, tx_fee: bool) -> OutputNote {
    OutputNote {
        note_type: note_type as i32,
        tx_fee,
        ..Default::default()
    }
}

#[tokio::test]
async fn note_policy_conforms() {
    let one = json!({ "max_payment_notes": 1 });
    let private = json!({ "deny_private_notes": true });
    Suite::new(note_policy::NotePolicy)
        .allows(
            "one payment and the fee note",
            notes(
                vec![note(NoteType::Public, false), note(NoteType::Public, true)],
                one.clone(),
            ),
        )
        .denies(
            "two payments",
            notes(
                vec![note(NoteType::Public, false), note(NoteType::Public, false)],
                one,
            ),
        )
        .denies(
            "a private payment",
            notes(vec![note(NoteType::Private, false)], private),
        )
        .request(
            "registration",
            registration(
                json!({}),
                &["0x0000000000000000000000000000000000000000000000000000000000000051"],
            ),
        )
        .valid_params(json!({ "max_payment_notes": 1, "deny_private_notes": true }))
        .valid_params(json!({}))
        .invalid_params(json!({ "max_notes": 1 }))
        .run()
        .await
        .assert_conformant();
}

#[tokio::test]
#[ignore = "timing report for the prototype gates; run with --ignored --nocapture"]
async fn loopback_evaluate_round_trip() {
    let mut client = GuardianModuleClient::new(start_remote().await);
    let request = payment(
        &[500],
        json!({ "max_per_transaction": { "0x0000000000000000000000000000aa": "1000" } }),
    );
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
        SpendingLimit.evaluate(&request).await.unwrap();
    }
    println!(
        "remote p50 {:?} p99 {:?}; in-process mean {:?}",
        samples[500],
        samples[990],
        local_started.elapsed() / 1000
    );
}
