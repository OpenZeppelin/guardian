use guardian_client::{AuthConfig, GuardianClient, MidenFalconRpoAuth, auth_config::AuthType};
use miden_protocol::Word;
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
///
/// Does nothing on the second pass. `det-restart-durability` lists this action
/// before its assertion, and the whole scenario runs again after the restart, so
/// registering there would recreate exactly the state the assertion is looking
/// for: the account would be present because this call had just put it back, not
/// because it survived. The same trap applies to an upgrade, where a wiped
/// database would read as a successful migration.
pub async fn register(runner: &Runner) -> ActionOutcome {
    if runner.post_restart {
        return ActionOutcome::Passed;
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

/// Whether two commitments name the same state, whatever their spelling.
///
/// GUARDIAN's rendering and the fixture's are both hex words, but nothing
/// guarantees the same case or the same `0x`, and a comparison that tripped on
/// either would fail for a reason that is not a defect.
fn same_commitment(left: &str, right: &str) -> bool {
    fn canonical(value: &str) -> String {
        value
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .to_ascii_lowercase()
    }
    let (left, right) = (canonical(left), canonical(right));
    !left.is_empty() && left == right
}

/// Reads the account back through GUARDIAN and checks the commitment it
/// reports is the one the registered state carries.
///
/// Compared against the fixture's own commitment rather than merely required
/// to be present. A GUARDIAN that stored a corrupted or stale state, or served
/// another account's, would answer with a perfectly well-formed commitment, and
/// a presence check would call that a pass on a required scenario.
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
    if !same_commitment(&account.commitment, &fixtures.initial_commitment) {
        return ActionOutcome::failed_product(format!(
            "GUARDIAN reports commitment `{}` for the fixture account, but the state it was \
             registered with carries `{}`",
            account.commitment, fixtures.initial_commitment
        ));
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
    // Same reason `register` no-ops after a restart: a pass that rewrites the
    // data it is checking proves nothing. Pushing the fixture proposal again
    // here would recreate whatever the restart lost, and `assert_durability`
    // would then find a proposal and pass. The proposal this scenario pushed
    // before the restart is still listed if it survived, which is what the
    // assertion reads.
    if runner.post_restart {
        return ActionOutcome::Passed;
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

    let nonce = fixtures.proposal_nonce();
    let payload = match fixtures.proposal_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "building the fixture proposal payload: {error}"
            ));
        }
    };

    if let Err(error) = client.push_delta_proposal(&id, nonce, &payload).await {
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

/// Ensures a fixture proposal is pending, so the post-restart pass has a
/// specific one to look for. Idempotent: a proposal already at the fixture
/// nonce is left alone.
async fn seed_proposal(
    client: &mut guardian_client::GuardianClient,
    id: &miden_protocol::account::AccountId,
    fixtures: &Fixtures,
) -> Result<(), ActionOutcome> {
    let nonce = fixtures.proposal_nonce();
    match client.get_delta_proposals(id).await {
        Ok(response)
            if response
                .proposals
                .iter()
                .any(|proposal| proposal.nonce == nonce) =>
        {
            return Ok(());
        }
        Ok(_) => {}
        Err(error) => {
            return Err(ActionOutcome::failed_product(format!(
                "listing proposals before the restart failed: {error}"
            )));
        }
    }

    let payload = match fixtures.proposal_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return Err(ActionOutcome::failed_setup(format!(
                "building the fixture proposal payload: {error}"
            )));
        }
    };
    client
        .push_delta_proposal(id, nonce, &payload)
        .await
        .map_err(|error| {
            ActionOutcome::failed_product(format!(
                "seeding a proposal before the restart failed: {error}"
            ))
        })?;
    Ok(())
}

/// Confirms the registered account is still readable after the server was
/// restarted, along with the proposal it was carrying.
///
/// Two phases, one scenario. Before the restart the account and its proposal
/// are trivially present, so passing there would prove nothing; that phase
/// seeds what the second one checks and skips. Only the post-restart phase
/// asserts.
pub async fn assert_durability(runner: &Runner) -> ActionOutcome {
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

    if !runner.post_restart {
        // Seeded here rather than by adding `proposal-create` to this
        // scenario's actions. Another scenario pushes the same fixture proposal
        // earlier in the pass, so this asks for one only when there is none:
        // pushing unconditionally would be a second push of the same account
        // and nonce, and depending on that other scenario having run would make
        // this assertion fail under `--scenario det-restart-durability` for a
        // reason that has nothing to do with durability.
        return match seed_proposal(&mut client, &id, fixtures).await {
            Ok(()) => ActionOutcome::Skipped {
                reason: "the server has not been restarted yet in this run; a proposal is \
                         seeded for the post-restart pass to look for"
                    .to_string(),
            },
            Err(outcome) => outcome,
        };
    }

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
            // An empty list is a successful response, so asking only whether
            // the call errored cannot tell a surviving proposal from a lost
            // one: it checks that GUARDIAN still answers, which the account
            // read above already established. The fixture proposal is
            // deterministic, so the pass that did not create it can still say
            // exactly which one it expects to find.
            let expected_nonce = fixtures.proposal_nonce();
            match client.get_delta_proposals(&id).await {
                Ok(response) => {
                    if response
                        .proposals
                        .iter()
                        .any(|proposal| proposal.nonce == expected_nonce)
                    {
                        ActionOutcome::Passed
                    } else {
                        ActionOutcome::failed_product(format!(
                            "the account survived the restart but its proposal at nonce \
                             {expected_nonce} did not: GUARDIAN lists {} proposal(s) [{}]",
                            response.proposals.len(),
                            response
                                .proposals
                                .iter()
                                .map(|proposal| proposal.nonce.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                }
                Err(error) => ActionOutcome::failed_product(format!(
                    "the account survived the restart but its proposal list could not be \
                     read: {error}"
                )),
            }
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

/// Exercises `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES` as configured, not as parsed.
///
/// Runs against the stack's third GUARDIAN, restricted to ECDSA. That is the
/// direction worth proving: the production guides recommend ECDSA-only because
/// it is what the hosted signer backends support, so an operator following them
/// is running exactly this configuration.
///
/// Both halves matter. A server that refused every registration would satisfy
/// the refusal on its own, so the allowed scheme has to get past the gate for
/// the refusal to mean anything. The allowed half asserts only that the gate did
/// not turn it away, not that registration succeeded: there is no committed
/// ECDSA fixture, so the attempt fails later on its credentials, and failing
/// later is precisely the evidence that the gate let it through.
pub async fn assert_scheme_gate(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let Ok(endpoint) = std::env::var("QUAL_GUARDIAN_SCHEME_GATED_GRPC") else {
        return ActionOutcome::EnvironmentBlocked {
            reason: "QUAL_GUARDIAN_SCHEME_GATED_GRPC is unset, so no scheme-gated GUARDIAN is \
                     running to exercise the gate against"
                .to_string(),
        };
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };

    // The blocked scheme. This registration would succeed on the other servers
    // in the stack, so the gate is the only thing that can turn it away.
    let falcon_signer = match fixtures.signer() {
        Ok(signer) => Arc::new(signer),
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "cannot build the fixture signer: {error}"
            ));
        }
    };
    let mut falcon_client = match GuardianClient::connect(endpoint.clone()).await {
        Ok(client) => client.with_signer(falcon_signer),
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "cannot reach the scheme-gated GUARDIAN at {endpoint}: {error}"
            ));
        }
    };
    let falcon_auth = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_commitments: fixtures.cosigner_commitments.clone(),
        })),
    };
    match falcon_client
        .configure(&id, falcon_auth, &fixtures.account)
        .await
    {
        Ok(_) => {
            return ActionOutcome::failed_product(
                "the scheme-gated GUARDIAN accepted a Falcon registration while configured to \
                 allow ECDSA only"
                    .to_string(),
            );
        }
        Err(error) => match error.guardian_code() {
            Some(code) if code == "signature_scheme_not_allowed" => {}
            Some(code) => {
                return ActionOutcome::failed_product(format!(
                    "the Falcon registration was refused with `{code}` rather than \
                     `signature_scheme_not_allowed`, so the gate is not what turned it away: \
                     {error}"
                ));
            }
            None => {
                return ActionOutcome::failed_product(format!(
                    "the Falcon registration failed without a GUARDIAN error code: {error}"
                ));
            }
        },
    }

    // The allowed scheme. It must not be refused by the gate; anything else is
    // this account not being an ECDSA account, which is expected.
    let ecdsa_signer = Arc::new(guardian_client::EcdsaKeyStore::generate());
    let mut ecdsa_client = match GuardianClient::connect(endpoint.clone()).await {
        Ok(client) => client.with_signer(ecdsa_signer),
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "cannot reach the scheme-gated GUARDIAN at {endpoint}: {error}"
            ));
        }
    };
    let ecdsa_auth = AuthConfig {
        auth_type: Some(AuthType::MidenEcdsa(guardian_client::MidenEcdsaAuth {
            cosigner_commitments: fixtures.cosigner_commitments.clone(),
        })),
    };
    match ecdsa_client
        .configure(&id, ecdsa_auth, &fixtures.account)
        .await
    {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => match error.guardian_code() {
            Some(code) if code == "signature_scheme_not_allowed" => ActionOutcome::failed_product(
                "the scheme-gated GUARDIAN refused an ECDSA registration although ECDSA is the \
                 scheme it allows, so the gate is turning away more than it should"
                    .to_string(),
            ),
            _ => ActionOutcome::Passed,
        },
    }
}

/// Proves a paused account refuses a proposal, through the operator surface an
/// operator would actually use.
///
/// Pause, attempt, unpause in one action on purpose. The fixture account is
/// shared by every scenario in the run, so a pause left behind would fail
/// whatever ran next for a reason that had nothing to do with it. Unpausing on
/// every path, including the failing ones, keeps that contained.
///
/// GUARDIAN enforces the pause at each write path and tests all of them, but
/// nothing previously drove a paused account from the outside, so the chain from
/// an operator's click to a refused proposal was only ever proven in pieces.
pub async fn assert_paused_account_refuses(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let id = match account_id(fixtures) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };

    let base = runner.endpoints.http.trim_end_matches('/').to_string();

    if let Err(outcome) = operator_login(runner, &base, fixtures).await {
        return outcome;
    }

    if let Err(outcome) = set_paused(runner, &base, &id.to_string(), true).await {
        return outcome;
    }

    let outcome = attempt_proposal_while_paused(runner, fixtures, id).await;

    // Restored whatever the attempt concluded: leaving the shared fixture
    // account paused would fail the next scenario for an unrelated reason.
    if let Err(unpause_failure) = set_paused(runner, &base, &id.to_string(), false).await {
        return match outcome {
            ActionOutcome::Passed => unpause_failure,
            other => other,
        };
    }

    outcome
}

pub(super) async fn operator_login(
    runner: &Runner,
    base: &str,
    fixtures: &Fixtures,
) -> Result<(), ActionOutcome> {
    let (operator, commitment) = fixtures.operator();

    let challenge: serde_json::Value = match runner
        .http
        .get(format!("{base}/auth/challenge"))
        .query(&[("commitment", commitment.as_str())])
        .send()
        .await
    {
        Ok(response) => match response.json().await {
            Ok(body) => body,
            Err(error) => {
                return Err(ActionOutcome::failed_product(format!(
                    "the operator challenge was not JSON: {error}"
                )));
            }
        },
        Err(error) => {
            return Err(ActionOutcome::failed_setup(format!(
                "requesting an operator challenge failed: {error}"
            )));
        }
    };

    let Some(digest) = challenge
        .pointer("/challenge/signing_digest")
        .and_then(serde_json::Value::as_str)
    else {
        return Err(ActionOutcome::failed_product(format!(
            "the operator challenge carried no signing_digest: {challenge}"
        )));
    };
    let digest = match Word::try_from(digest) {
        Ok(word) => word,
        Err(error) => {
            return Err(ActionOutcome::failed_product(format!(
                "the challenge signing_digest is not a word: {error}"
            )));
        }
    };

    let signature = format!(
        "0x{}",
        hex::encode(miden_protocol::utils::serde::Serializable::to_bytes(
            &operator.sign(digest)
        ))
    );

    match runner
        .http
        .post(format!("{base}/auth/verify"))
        .json(&serde_json::json!({ "commitment": commitment, "signature": signature }))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(ActionOutcome::failed_product(format!(
            "the operator session was refused with {}",
            response.status()
        ))),
        Err(error) => Err(ActionOutcome::failed_setup(format!(
            "verifying the operator challenge failed: {error}"
        ))),
    }
}

pub(super) async fn set_paused(
    runner: &Runner,
    base: &str,
    account_id: &str,
    paused: bool,
) -> Result<(), ActionOutcome> {
    let route = if paused { "pause" } else { "unpause" };
    let body = serde_json::json!({ "reason": "qualification: paused-account scenario" });
    match runner
        .http
        .post(format!("{base}/dashboard/accounts/{account_id}/{route}"))
        .json(&body)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(ActionOutcome::failed_product(format!(
            "{route} was refused with {}",
            response.status()
        ))),
        Err(error) => Err(ActionOutcome::failed_setup(format!(
            "calling {route} failed: {error}"
        ))),
    }
}

async fn attempt_proposal_while_paused(
    runner: &Runner,
    fixtures: &Fixtures,
    id: AccountId,
) -> ActionOutcome {
    let mut client = match connect(runner, fixtures).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };
    let payload = match fixtures.proposal_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "building the fixture proposal payload: {error}"
            ));
        }
    };

    // A different nonce from `det-proposal-lifecycle`, so a proposal that scenario
    // already pushed cannot make this one look refused for being a duplicate.
    let nonce = fixtures.proposal_nonce() + 1;
    match client.push_delta_proposal(&id, nonce, &payload).await {
        Ok(_) => ActionOutcome::failed_product("a paused account accepted a proposal".to_string()),
        Err(error) => match error.guardian_code() {
            Some(code) if code == "GUARDIAN_ACCOUNT_PAUSED" => ActionOutcome::Passed,
            Some(code) => ActionOutcome::failed_product(format!(
                "the proposal was refused with `{code}` rather than `GUARDIAN_ACCOUNT_PAUSED`, \
                 so the pause is not what stopped it: {error}"
            )),
            None => ActionOutcome::failed_product(format!(
                "the proposal failed without a GUARDIAN error code: {error}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::same_commitment;

    /// The fixture's own commitment, and the same value spelled differently.
    const FIXTURE: &str = "0x552759e44efe4db81e0e699f5aea5b04b099e3bce84f4e369e4de0bb5ebb9cd9";

    #[test]
    fn spelling_does_not_decide_the_answer() {
        assert!(same_commitment(FIXTURE, &FIXTURE.to_ascii_uppercase()));
        assert!(same_commitment(FIXTURE, FIXTURE.trim_start_matches("0x")));
        assert!(same_commitment(&format!("  {FIXTURE}  "), FIXTURE));
    }

    /// The case the presence check could not see: a well-formed commitment that
    /// is not the one the account was registered with.
    #[test]
    fn a_different_well_formed_commitment_is_refused() {
        let other = "0xd49fcc29db562df747ff38ec96aeb4e20f2965d4cad72952dfef7b922ca7cff0";
        assert!(!same_commitment(other, FIXTURE));
    }

    #[test]
    fn nothing_is_not_a_match() {
        assert!(!same_commitment("", ""));
        assert!(!same_commitment("0x", FIXTURE));
    }
}
