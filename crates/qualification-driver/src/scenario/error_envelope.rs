use std::sync::Arc;

use super::{ActionOutcome, Runner};
use guardian_client::{FalconKeyStore, GuardianClient};
use miden_protocol::account::AccountId;

/// Syntactically valid and deliberately never registered. The account lookup
/// runs before timestamp validation and before signature verification, so a
/// throwaway credential still reaches the envelope this asserts.
const UNREGISTERED_ACCOUNT_ID: &str = "0xaabbccddeeff00011b27a8df4ddbe0";
const EXPECTED_CODE: &str = "account_not_found";

pub async fn assert_grpc_envelope(runner: &Runner) -> ActionOutcome {
    let account_id = match AccountId::from_hex(UNREGISTERED_ACCOUNT_ID) {
        Ok(id) => id,
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "the fixture account id is malformed: {error}"
            ));
        }
    };

    // Credentials are extracted before the account is looked up, so an
    // unauthenticated request is rejected at the boundary and never reaches the
    // envelope this asserts. A throwaway signer gets past that.
    let signer = Arc::new(FalconKeyStore::generate());
    let mut client = match GuardianClient::connect(runner.endpoints.grpc.clone()).await {
        Ok(client) => client.with_signer(signer),
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "cannot reach {}: {error}",
                runner.endpoints.grpc
            ));
        }
    };

    match client.get_state(&account_id).await {
        Ok(_) => ActionOutcome::failed_product(
            "an unregistered account returned state instead of an error".to_string(),
        ),
        Err(error) => match error.guardian_code() {
            Some(code) if code == EXPECTED_CODE => ActionOutcome::Passed,
            Some(code) => ActionOutcome::failed_product(format!(
                "expected code `{EXPECTED_CODE}` over gRPC, got `{code}`"
            )),
            None => ActionOutcome::failed_product(format!(
                "gRPC error carried no structured envelope: {error}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed fixture would make the assertion fail while parsing instead
    /// of while reading the envelope, which looks like a product defect.
    #[test]
    fn the_unregistered_account_id_is_structurally_valid() {
        AccountId::from_hex(UNREGISTERED_ACCOUNT_ID)
            .expect("the unregistered fixture account id must parse");
    }
}
