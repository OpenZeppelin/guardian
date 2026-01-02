use crate::error::{PsmError, Result};
use base64::Engine;
use serde_json::{Map, Value};

const VALID_PROPOSAL_TYPES: &[&str] = &[
    "add_signer",
    "remove_signer",
    "change_threshold",
    "switch_psm",
    "consume_notes",
    "p2id",
];

pub fn normalize_payload(payload: Value) -> Result<Value> {
    let mut obj = payload
        .as_object()
        .cloned()
        .ok_or_else(|| PsmError::InvalidDelta("delta_payload must be an object".to_string()))?;

    let tx_summary = obj
        .get("tx_summary")
        .ok_or_else(|| PsmError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;
    validate_tx_summary(tx_summary)?;

    if let Some(metadata) = obj.remove("metadata") {
        let normalized_metadata = normalize_metadata(metadata)?;
        obj.insert("metadata".to_string(), normalized_metadata);
    }

    Ok(Value::Object(obj))
}

fn validate_tx_summary(tx_summary: &Value) -> Result<()> {
    let obj = tx_summary.as_object().ok_or_else(|| {
        PsmError::InvalidDelta("tx_summary must be an object with 'data' field".to_string())
    })?;

    let data = obj
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| PsmError::InvalidDelta("tx_summary.data must be a string".to_string()))?;

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| PsmError::InvalidDelta(format!("tx_summary.data is not valid base64: {e}")))?;
    Ok(())
}

fn normalize_metadata(metadata: Value) -> Result<Value> {
    let mut obj = metadata
        .as_object()
        .cloned()
        .ok_or_else(|| PsmError::InvalidDelta("metadata must be a JSON object".to_string()))?;

    let proposal_type = match obj.get("proposal_type").and_then(Value::as_str) {
        Some(p) if VALID_PROPOSAL_TYPES.contains(&p) => p.to_string(),
        Some(p) => {
            return Err(PsmError::InvalidDelta(format!(
                "Unknown proposal_type '{}'. Must be one of: {}",
                p,
                VALID_PROPOSAL_TYPES.join(", ")
            )));
        }
        None => infer_proposal_type(&obj)?,
    };
    obj.insert("proposal_type".to_string(), Value::String(proposal_type));

    obj.entry("description")
        .or_insert_with(|| Value::String(String::new()));

    if let Some(amount) = obj.get("amount") {
        if let Some(num) = amount.as_u64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        } else if let Some(num) = amount.as_i64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        }
    }

    Ok(Value::Object(obj))
}

fn infer_proposal_type(obj: &Map<String, Value>) -> Result<String> {
    if obj.contains_key("recipient_id")
        || obj.contains_key("faucet_id")
        || obj.contains_key("amount")
    {
        return Ok("p2id".to_string());
    }
    if obj
        .get("note_ids")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        return Ok("consume_notes".to_string());
    }
    if obj.contains_key("new_psm_pubkey") {
        return Ok("switch_psm".to_string());
    }
    if obj.contains_key("signer_commitments") || obj.contains_key("target_threshold") {
        return Ok("change_threshold".to_string());
    }

    Err(PsmError::InvalidDelta(
        "Cannot determine proposal_type from metadata fields. Please provide an explicit proposal_type.".to_string(),
    ))
}
