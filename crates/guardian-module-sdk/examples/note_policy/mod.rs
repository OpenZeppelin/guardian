//! The `note-policy` module: limits how many payment notes a transaction may create and can
//! refuse private notes, whose recipients the Guardian cannot see. Shared by the gRPC example
//! (`note-policy-serve`) and the in-process one (`guardian-server`'s `custom_module_server`).

// Each including example uses only part of this file.
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleFactory, ModuleInfo,
    ModuleInstanceConfig, NoteType, PROTOCOL_VERSION, ParamsVerdict,
};
use serde::Deserialize;

pub const KIND: &str = "note-policy";

/// The module's own version, reported by `describe`. Not the crate version: this file is
/// compiled into both the SDK example and a server build.
pub const VERSION: &str = "1.0.0";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    max_payment_notes: Option<u32>,
    #[serde(default)]
    deny_private_notes: bool,
}

fn parse(params: &[u8]) -> Result<Params, String> {
    serde_json::from_slice(params).map_err(|e| e.to_string())
}

pub struct NotePolicy;

#[async_trait]
impl Module for NotePolicy {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: KIND.into(),
            version: VERSION.into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![
                Hook::BeforeProposalCreated as i32,
                Hook::BeforeTransactionApproved as i32,
            ],
            ..Default::default()
        })
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(match parse(params) {
            Ok(_) => ParamsVerdict::ok(),
            Err(message) => ParamsVerdict::invalid(message),
        })
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        let params = parse(&request.params).map_err(ModuleError::InvalidRequest)?;
        let Some(hook_request::Subject::Transaction(subject)) = &request.subject else {
            return Ok(Decision::allow());
        };
        let Some(projection) = subject.miden() else {
            return Err(ModuleError::InvalidRequest(
                "not a Miden transaction".into(),
            ));
        };
        let payments: Vec<_> = projection
            .output_notes
            .iter()
            .filter(|note| !note.tx_fee)
            .collect();

        if params.deny_private_notes
            && payments
                .iter()
                .any(|note| note.note_type == NoteType::Private as i32)
        {
            return Ok(Decision::deny(
                "private_note_not_allowed",
                "the transaction creates a private note",
            ));
        }
        if let Some(max) = params.max_payment_notes
            && payments.len() > max as usize
        {
            return Ok(Decision::deny(
                "too_many_payment_notes",
                format!("{} payment notes, at most {max} allowed", payments.len()),
            ));
        }
        Ok(Decision::allow())
    }
}

/// Creates the module for a catalog instance of kind `note-policy` running in process.
pub struct NotePolicyFactory;

impl ModuleFactory for NotePolicyFactory {
    fn create(&self, _instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
        Ok(Arc::new(NotePolicy))
    }
}

#[cfg(test)]
mod tests {
    use guardian_module_sdk::{
        MidenTransaction, OutputNote, TransactionProjection, TransactionSubject,
    };

    use super::*;

    fn request(params: serde_json::Value, notes: Vec<OutputNote>) -> HookRequest {
        HookRequest {
            params: serde_json::to_vec(&params).unwrap(),
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
    async fn counts_payments_but_not_the_fee_note() {
        let params = serde_json::json!({ "max_payment_notes": 1 });
        let one_payment = vec![note(NoteType::Public, false), note(NoteType::Public, true)];
        assert!(
            NotePolicy
                .evaluate(&request(params.clone(), one_payment))
                .await
                .unwrap()
                .allow
        );
        let two_payments = vec![note(NoteType::Public, false), note(NoteType::Public, false)];
        let decision = NotePolicy
            .evaluate(&request(params, two_payments))
            .await
            .unwrap();
        assert_eq!(decision.reason_code, "too_many_payment_notes");
    }

    #[tokio::test]
    async fn refuses_private_notes_when_asked() {
        let notes = vec![note(NoteType::Private, false)];
        let decision = NotePolicy
            .evaluate(&request(
                serde_json::json!({ "deny_private_notes": true }),
                notes.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(decision.reason_code, "private_note_not_allowed");
        assert!(
            NotePolicy
                .evaluate(&request(serde_json::json!({}), notes))
                .await
                .unwrap()
                .allow
        );
    }

    #[tokio::test]
    async fn unknown_params_are_invalid() {
        let verdict = NotePolicy
            .validate_params(br#"{"max_notes": 1}"#)
            .await
            .unwrap();
        assert!(!verdict.ok);
    }
}
