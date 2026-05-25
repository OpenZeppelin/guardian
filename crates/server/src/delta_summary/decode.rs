//! Payload normalization for the dashboard delta decoder.
//!
//! See research.md Decision 10 for the two persisted shapes this
//! resolves.

use guardian_shared::FromJson;
use miden_protocol::transaction::TransactionSummary;
use serde_json::Value;

use super::{DecodeSection, DecodeWarning, MultisigMetadata, NormalizedPayload};

/// Normalize a raw `delta_payload` JSON blob into a [`NormalizedPayload`].
///
/// Returns the normalized value plus any non-fatal warnings encountered
/// while decoding sub-sections (e.g., malformed metadata that did not
/// prevent the `tx_summary` decode). A fully-unrecognized payload
/// returns [`NormalizedPayload::Opaque`] with an empty warning vec —
/// the "unrecognized payload shape" is the result, not a warning.
pub fn resolve_payload(payload: &Value) -> (NormalizedPayload, Vec<DecodeWarning>) {
    if let Some(tx_summary_value) = payload.get("tx_summary") {
        return resolve_wrapper(tx_summary_value, payload.get("metadata"));
    }

    if payload.get("data").and_then(Value::as_str).is_some() {
        return resolve_raw(payload);
    }

    (
        NormalizedPayload::Opaque {
            reason: "unrecognized_payload_shape",
        },
        Vec::new(),
    )
}

fn resolve_wrapper(tx_summary_value: &Value, metadata_value: Option<&Value>) -> (NormalizedPayload, Vec<DecodeWarning>) {
    let mut warnings = Vec::new();

    let summary = match TransactionSummary::from_json(tx_summary_value) {
        Ok(s) => s,
        Err(err) => {
            return (
                NormalizedPayload::Opaque {
                    reason: classify_decode_error(&err),
                },
                Vec::new(),
            );
        }
    };

    let metadata = metadata_value.and_then(|value| match parse_metadata(value) {
        Ok(meta) => Some(meta),
        Err(reason) => {
            warnings.push(DecodeWarning {
                section: DecodeSection::Metadata,
                reason: reason.to_string(),
            });
            None
        }
    });

    (NormalizedPayload::WithSummary { summary, metadata }, warnings)
}

fn resolve_raw(payload: &Value) -> (NormalizedPayload, Vec<DecodeWarning>) {
    match TransactionSummary::from_json(payload) {
        Ok(summary) => (
            NormalizedPayload::WithSummary {
                summary,
                metadata: None,
            },
            Vec::new(),
        ),
        Err(err) => (
            NormalizedPayload::Opaque {
                reason: classify_decode_error(&err),
            },
            Vec::new(),
        ),
    }
}

fn parse_metadata(value: &Value) -> Result<MultisigMetadata, &'static str> {
    let obj = value.as_object().ok_or("metadata_not_object")?;
    let proposal_type = obj
        .get("proposal_type")
        .and_then(Value::as_str)
        .ok_or("missing_proposal_type")?
        .to_string();
    let recipient_id = obj.get("recipient_id").and_then(Value::as_str).map(str::to_string);
    let faucet_id = obj.get("faucet_id").and_then(Value::as_str).map(str::to_string);
    let amount = obj.get("amount").and_then(Value::as_str).map(str::to_string);
    let note_ids = obj
        .get("note_ids")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(MultisigMetadata {
        proposal_type,
        recipient_id,
        faucet_id,
        amount,
        note_ids,
    })
}

fn classify_decode_error(err: &str) -> &'static str {
    if err.contains("Base64") {
        "malformed_base64"
    } else if err.contains("Missing or invalid 'data' field") {
        "missing_data_field"
    } else {
        "malformed_tx_summary"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_summary::tests::fixtures;

    #[test]
    fn wrapper_with_valid_summary_resolves_with_metadata() {
        let payload = fixtures::multisig_p2id_wrapper();
        let (normalized, warnings) = resolve_payload(&payload);
        assert!(warnings.is_empty());
        match normalized {
            NormalizedPayload::WithSummary { metadata, .. } => {
                let metadata = metadata.expect("metadata extracted");
                assert_eq!(metadata.proposal_type, "p2id");
            }
            NormalizedPayload::Opaque { reason } => panic!("expected WithSummary, got Opaque({reason})"),
        }
    }

    #[test]
    fn wrapper_with_add_signer_metadata_resolves() {
        let payload = fixtures::multisig_add_signer();
        let (normalized, _warnings) = resolve_payload(&payload);
        match normalized {
            NormalizedPayload::WithSummary { metadata, .. } => {
                let metadata = metadata.expect("metadata extracted");
                assert_eq!(metadata.proposal_type, "add_signer");
            }
            NormalizedPayload::Opaque { .. } => panic!("expected WithSummary"),
        }
    }

    #[test]
    fn raw_push_delta_resolves_without_metadata() {
        let payload = fixtures::push_delta_raw_tx_summary();
        let (normalized, warnings) = resolve_payload(&payload);
        assert!(warnings.is_empty());
        match normalized {
            NormalizedPayload::WithSummary { metadata, .. } => assert!(metadata.is_none()),
            NormalizedPayload::Opaque { reason } => panic!("expected WithSummary, got Opaque({reason})"),
        }
    }

    #[test]
    fn evm_placeholder_resolves_opaque() {
        let payload = fixtures::evm_placeholder();
        let (normalized, _warnings) = resolve_payload(&payload);
        match normalized {
            NormalizedPayload::Opaque { reason } => assert_eq!(reason, "unrecognized_payload_shape"),
            NormalizedPayload::WithSummary { .. } => panic!("expected Opaque"),
        }
    }

    #[test]
    fn malformed_base64_resolves_opaque_with_decode_reason() {
        let payload = fixtures::malformed_base64();
        let (normalized, _warnings) = resolve_payload(&payload);
        match normalized {
            NormalizedPayload::Opaque { reason } => assert_eq!(reason, "malformed_base64"),
            NormalizedPayload::WithSummary { .. } => panic!("expected Opaque"),
        }
    }

    #[test]
    fn switch_guardian_resolves() {
        let payload = fixtures::multisig_switch_guardian();
        let (normalized, _warnings) = resolve_payload(&payload);
        match normalized {
            NormalizedPayload::WithSummary { metadata, .. } => {
                assert_eq!(metadata.unwrap().proposal_type, "switch_guardian");
            }
            NormalizedPayload::Opaque { .. } => panic!("expected WithSummary"),
        }
    }
}
