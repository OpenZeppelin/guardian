use guardian_compliance_module::{Compliance, Lists};
use guardian_module_sdk::conformance::Suite;
use guardian_module_sdk::proto::{asset, hook_request};
use guardian_module_sdk::{
    AccountStateProjection, AccountView, Asset, HookRequest, MidenAccountState, MidenTransaction,
    OutputNote, RegistrationSubject, TransactionProjection, TransactionSubject,
};
use serde_json::json;

const VERIFIED_SIGNER: &str = "0x0000000000000000000000000000000000000000000000000000000000000051";
const OTHER_VERIFIED_SIGNER: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000052";
const UNVERIFIED_SIGNER: &str =
    "0x00000000000000000000000000000000000000000000000000000000000000ff";
const PAYER: &str = "0x0000000000000000000000000000f1";
const SANCTIONED: &str = "0x000000000000000000000000000bad";
const VERIFIED_ACCOUNT: &str = "0x0000000000000000000000000000a1";
const UNKNOWN_ACCOUNT: &str = "0x0000000000000000000000000000c3";

fn lists() -> Lists {
    Lists::from_json(&format!(
        r#"{{
            "kyc_signers": ["{VERIFIED_SIGNER}", "{OTHER_VERIFIED_SIGNER}"],
            "sanctioned_accounts": ["{SANCTIONED}"],
            "verified_accounts": ["{VERIFIED_ACCOUNT}"]
        }}"#
    ))
    .unwrap()
}

fn view(signers: &[&str]) -> AccountView {
    AccountView {
        signer_commitments: signers.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn registration(params: serde_json::Value, signers: &[&str]) -> HookRequest {
    HookRequest {
        account_id: PAYER.into(),
        params: serde_json::to_vec(&params).unwrap(),
        after: Some(view(signers)),
        subject: Some(hook_request::Subject::Registration(RegistrationSubject {
            initial_state: Some(AccountStateProjection::miden(MidenAccountState::default())),
        })),
        ..Default::default()
    }
}

fn payment_to(recipient: &str) -> OutputNote {
    OutputNote {
        note_id: format!("0xnote{recipient}"),
        assets: vec![Asset {
            faucet_id: "0x0000000000000000000000000000aa".into(),
            value: Some(asset::Value::FungibleAmount("1".into())),
        }],
        p2id_target: Some(recipient.into()),
        ..Default::default()
    }
}

fn transaction(
    params: serde_json::Value,
    before: &[&str],
    after: &[&str],
    notes: Vec<OutputNote>,
) -> HookRequest {
    HookRequest {
        account_id: PAYER.into(),
        params: serde_json::to_vec(&params).unwrap(),
        before: Some(view(before)),
        after: Some(view(after)),
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

fn paying(params: serde_json::Value, notes: Vec<OutputNote>) -> HookRequest {
    transaction(params, &[VERIFIED_SIGNER], &[VERIFIED_SIGNER], notes)
}

#[tokio::test]
async fn signers_must_be_kyc_verified() {
    let added = json!({ "signers": "added" });
    let unverified_upper = format!("0x{}", UNVERIFIED_SIGNER[2..].to_ascii_uppercase());
    Suite::new(Compliance::new(lists()))
        .allows(
            "registration with verified signers",
            registration(json!({}), &[VERIFIED_SIGNER, OTHER_VERIFIED_SIGNER]),
        )
        .denies_with(
            "registration with an unverified signer",
            registration(json!({}), &[VERIFIED_SIGNER, UNVERIFIED_SIGNER]),
            "signer_not_verified",
        )
        .denies_with(
            "registration with an unverified signer, even when only added signers are checked later",
            registration(added.clone(), &[UNVERIFIED_SIGNER]),
            "signer_not_verified",
        )
        .denies_with(
            "a transaction adding an unverified signer",
            transaction(
                added.clone(),
                &[VERIFIED_SIGNER],
                &[VERIFIED_SIGNER, UNVERIFIED_SIGNER],
                vec![],
            ),
            "signer_not_verified",
        )
        .allows(
            "a transaction of an account whose unverified signer predates the check, when only added signers are checked",
            transaction(added.clone(), &[UNVERIFIED_SIGNER], &[UNVERIFIED_SIGNER], vec![]),
        )
        .allows(
            "a signer spelled in another hex case before and after is not added",
            transaction(added, &[&unverified_upper], &[UNVERIFIED_SIGNER], vec![]),
        )
        .denies_with(
            "any transaction of an account with an unverified signer, when every signer is checked",
            transaction(json!({}), &[UNVERIFIED_SIGNER], &[UNVERIFIED_SIGNER], vec![]),
            "signer_not_verified",
        )
        .allows(
            "signer checks off",
            registration(json!({ "signers": "off" }), &[UNVERIFIED_SIGNER]),
        )
        .fails(
            "a registration without the account's signers",
            registration(json!({}), &[]),
        )
        .valid_params(json!({}))
        .valid_params(json!({ "signers": "added", "recipients": "allowlist" }))
        .invalid_params(json!({ "signers": "some" }))
        .invalid_params(json!({ "sanctions": true }))
        .run()
        .await
        .assert_conformant();
}

#[tokio::test]
async fn recipients_are_screened() {
    let allowlist = json!({ "recipients": "allowlist" });
    let reclaimable = |reclaimer: &str| OutputNote {
        reclaimer: Some(reclaimer.into()),
        ..payment_to(VERIFIED_ACCOUNT)
    };
    let fee = OutputNote {
        tx_fee: true,
        ..payment_to(SANCTIONED)
    };
    let partial = OutputNote {
        partial: true,
        ..payment_to(VERIFIED_ACCOUNT)
    };
    let custom_script = OutputNote {
        p2id_target: None,
        ..payment_to(VERIFIED_ACCOUNT)
    };
    let mut without_projection = paying(json!({}), vec![payment_to(UNKNOWN_ACCOUNT)]);
    without_projection.subject = Some(hook_request::Subject::Transaction(
        TransactionSubject::default(),
    ));
    Suite::new(Compliance::new(lists()))
        .allows(
            "a payment to an account that is not sanctioned",
            paying(json!({}), vec![payment_to(UNKNOWN_ACCOUNT)]),
        )
        .denies_with(
            "a payment to a sanctioned account",
            paying(json!({}), vec![payment_to(SANCTIONED)]),
            "recipient_sanctioned",
        )
        .denies_with(
            "a reclaimable note whose reclaimer is sanctioned",
            paying(json!({}), vec![reclaimable(SANCTIONED)]),
            "recipient_sanctioned",
        )
        .allows(
            "a reclaimable note the payer can take back, under the allowlist where the payer is not listed",
            paying(allowlist.clone(), vec![reclaimable(PAYER)]),
        )
        .allows(
            "the fee note is not a payment",
            paying(json!({}), vec![fee, payment_to(UNKNOWN_ACCOUNT)]),
        )
        .denies_with(
            "a note whose recipient cannot be read",
            paying(json!({}), vec![partial]),
            "recipient_unknown",
        )
        .denies_with(
            "a full note with a script the Guardian does not read a recipient from",
            paying(json!({}), vec![custom_script]),
            "recipient_unknown",
        )
        .denies_with(
            "an id in another case is the same account",
            paying(json!({}), vec![payment_to(&SANCTIONED.to_uppercase().replacen("0X", "0x", 1))]),
            "recipient_sanctioned",
        )
        .fails("a transaction without its projection", without_projection)
        .allows(
            "a payment to a verified account under the allowlist",
            paying(allowlist.clone(), vec![payment_to(VERIFIED_ACCOUNT)]),
        )
        .denies_with(
            "a payment to an account that is not verified under the allowlist",
            paying(allowlist, vec![payment_to(UNKNOWN_ACCOUNT)]),
            "recipient_not_verified",
        )
        .allows(
            "recipient checks off",
            paying(json!({ "recipients": "off" }), vec![payment_to(SANCTIONED)]),
        )
        .valid_params(json!({ "recipients": "blocklist" }))
        .invalid_params(json!({ "recipients": "denylist" }))
        .run()
        .await
        .assert_conformant();
}

#[test]
fn a_data_file_with_an_id_that_could_never_match_is_refused() {
    assert!(Lists::from_json(r#"{ "kyc_signers": ["0x51"] }"#).is_err());
    assert!(
        Lists::from_json(r#"{ "sanctioned_accounts": ["mtst1qrkd5x7qy0mw0yqzlvqlqd3w9cafjkw"] }"#)
            .is_err()
    );
    assert!(Lists::from_json(r#"{ "sanction_list": [] }"#).is_err());
    assert_eq!(
        Lists::from_json(r#"{ "verified_accounts": ["0x0000000000000000000000000000A1"] }"#)
            .unwrap()
            .sizes(),
        (0, 0, 1)
    );
}
