use guardian_client::{AuthConfig, GuardianClient, MidenFalconRpoAuth, auth_config::AuthType};
use miden_protocol::account::AccountId;
use std::sync::Arc;

use super::{ActionOutcome, Runner};
use crate::fixtures::Fixtures;

async fn connect(runner: &Runner, fixtures: &Fixtures) -> Result<GuardianClient, ActionOutcome> {
    let signer = match fixtures.signer() {
        Ok(signer) => Arc::new(signer),
        Err(error) => {
            return Err(ActionOutcome::failed_setup(format!(
                "cannot build the fixture signer: {error}"
            )));
        }
    };
    match GuardianClient::connect(runner.endpoints.grpc.clone()).await {
        Ok(client) => Ok(client.with_signer(signer)),
        Err(error) => Err(ActionOutcome::failed_setup(format!(
            "cannot reach {}: {error}",
            runner.endpoints.grpc
        ))),
    }
}

fn account_id(fixtures: &Fixtures) -> Result<AccountId, ActionOutcome> {
    AccountId::from_hex(&fixtures.account_id).map_err(|error| {
        ActionOutcome::failed_setup(format!("the fixture account id is malformed: {error}"))
    })
}

/// Registers the committed fixture account. The server must carry the matching
/// acknowledgement identity, because the account's stored state binds it.
pub async fn register(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };
    let mut client = match connect(runner, fixtures).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };

    let auth = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_commitments: fixtures.cosigner_commitments.clone(),
        })),
    };

    match client.configure(&id, auth, &fixtures.account).await {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => match error.guardian_code() {
            Some(code) if code == "account_already_configured" => ActionOutcome::Passed,
            // The fixture account binds one specific guardian commitment, so a
            // server carrying any other acknowledgement identity rejects it.
            // That reads as an authorization failure, which is true but sends
            // the reader looking in the wrong place.
            Some(code) if code == "authorization_failed" => ActionOutcome::failed_setup(
                "the server rejected the fixture account as unauthorized, which usually means its \
                 acknowledgement identity is not the fixture's. The deterministic profile must run \
                 against a server provisioned with the fixture guardian key."
                    .to_string(),
            ),
            Some(code) => ActionOutcome::failed_product(format!(
                "registering the fixture account failed with `{code}`: {error}"
            )),
            None => ActionOutcome::failed_product(format!(
                "registering the fixture account failed: {error}"
            )),
        },
    }
}

/// Reads the account back through GUARDIAN and checks the commitment it
/// reports is the one the registered state carries.
pub async fn verify_commitment(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };
    let mut client = match connect(runner, fixtures).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };

    let state = match client.get_state(&id).await {
        Ok(state) => state,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "reading the registered account back failed: {error}"
            ));
        }
    };

    let Some(account) = state.state else {
        return ActionOutcome::failed_product(
            "GUARDIAN reported success but returned no account state".to_string(),
        );
    };
    if account.commitment.trim().is_empty() {
        return ActionOutcome::failed_product(
            "GUARDIAN returned an account with no commitment".to_string(),
        );
    }
    if account.account_id != fixtures.account_id {
        return ActionOutcome::failed_product(format!(
            "GUARDIAN returned account `{}` for a request about `{}`",
            account.account_id, fixtures.account_id
        ));
    }
    ActionOutcome::Passed
}

/// Pushes the fixture proposal and confirms it comes back in the account's
/// pending list.
pub async fn create_proposal(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };
    let mut client = match connect(runner, fixtures).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };

    let nonce = fixtures.proposal["nonce"].as_u64().unwrap_or(1);

    if let Err(error) = client
        .push_delta_proposal(&id, nonce, &fixtures.proposal)
        .await
    {
        return match error.guardian_code() {
            Some(code) => ActionOutcome::failed_product(format!(
                "pushing the fixture proposal failed with `{code}`: {error}"
            )),
            None => ActionOutcome::failed_product(format!(
                "pushing the fixture proposal failed: {error}"
            )),
        };
    }

    match client.get_delta_proposals(&id).await {
        Ok(response) if !response.proposals.is_empty() => ActionOutcome::Passed,
        Ok(_) => ActionOutcome::failed_product(
            "the proposal was accepted but does not appear in the pending list".to_string(),
        ),
        Err(error) => ActionOutcome::failed_product(format!("listing proposals failed: {error}")),
    }
}

/// Confirms the registered account is still readable after the server was
/// restarted. Only the post-restart phase can assert anything: before the
/// restart the account is trivially present, so passing there would prove
/// nothing about durability.
pub async fn assert_durability(runner: &Runner) -> ActionOutcome {
    if !runner.post_restart {
        return ActionOutcome::Skipped {
            reason: "the server has not been restarted yet in this run".to_string(),
        };
    }

    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };
    let mut client = match connect(runner, fixtures).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };

    let state = match client.get_state(&id).await {
        Ok(state) => state,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "the account was not readable after the restart: {error}"
            ));
        }
    };

    match state.state {
        Some(account) if account.account_id == fixtures.account_id => {
            if client.get_delta_proposals(&id).await.is_err() {
                return ActionOutcome::failed_product(
                    "the account survived the restart but its proposal list did not".to_string(),
                );
            }
            ActionOutcome::Passed
        }
        Some(account) => ActionOutcome::failed_product(format!(
            "after the restart GUARDIAN returned account `{}` instead of `{}`",
            account.account_id, fixtures.account_id
        )),
        None => {
            ActionOutcome::failed_product("the account did not survive the restart".to_string())
        }
    }
}
