//! Policy modules, driven the way an operator and a client meet them: instances from the
//! stack's catalog (`qualification/stack/lib/modules.sh`), assigned to an account through the
//! operator API, refusing through the SDK.
//!
//! Every instance in that catalog is scoped `assigned`, so it applies only to an account a
//! scenario assigns it to. The deterministic actions assign to the fixture account every
//! scenario shares, so each one clears its assignment on every path, as the pause scenario
//! unpauses.
//!
//! They propose the first threshold-only fixture delta (`queue_1`) rather than `delta_1`: the
//! fixture generator adds `delta_1`'s signer without the approver scheme entry a real
//! add-signer writes, and the Guardian refuses to build a module's account view from that.

use std::sync::Arc;

use guardian_client::{ClientError, GuardianClient, ModuleRefusal};
use guardian_module_sdk::policies::SignerPolicy;
use miden_multisig_client::{MultisigError, TransactionType};
use serde_json::{Value, json};

use super::{ActionOutcome, Runner};
use crate::fixtures::Fixtures;

/// The instances the stack's catalog declares.
const GATE: &str = "qual-gate";
const CAP: &str = "qual-cap";
const SWITCH: &str = "qual-switch";
const REMOTE: &str = "qual-remote";
const RECIPIENT: &str = "qual-recipient";

/// A signer commitment nobody holds. Checked against every signer, an allowlist of only this
/// refuses every transaction of the fixture account.
const UNLISTED_SIGNER: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

const REASON: &str = "qualification: policy module scenario";

/// Retries `$call` while GUARDIAN still holds an earlier change of the account as pending,
/// which it reports before any module runs.
macro_rules! settled {
    ($call:expr) => {{
        let started = std::time::Instant::now();
        let mut wait = std::time::Duration::from_secs(1);
        loop {
            match $call.await {
                Err(error)
                    if error.to_string().contains("already a pending change")
                        && started.elapsed() < super::live::SETTLE_DEADLINE =>
                {
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(std::time::Duration::from_secs(5));
                }
                other => break other,
            }
        }
    }};
}

fn refuse_every_signer() -> Value {
    json!({ "check": "all", "allowlist": [UNLISTED_SIGNER] })
}

/// The operator API, logged in as the operator holding `policies:write` and
/// `policies:write_global` (enable and disable apply to every account).
struct Operator<'a> {
    runner: &'a Runner,
    base: String,
}

impl<'a> Operator<'a> {
    async fn login(runner: &'a Runner, fixtures: &Fixtures) -> Result<Self, ActionOutcome> {
        let base = runner.endpoints.http.trim_end_matches('/').to_string();
        super::account::operator_login(runner, &base, fixtures).await?;
        Ok(Self { runner, base })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ActionOutcome> {
        let url = format!("{}/dashboard{path}", self.base);
        let mut request = self.runner.http.request(method.clone(), &url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            ActionOutcome::failed_setup(format!("calling {method} {path} failed: {error}"))
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ActionOutcome::failed_product(format!(
                "{method} {path} was refused with {status}: {text}"
            )));
        }
        serde_json::from_str(&text).map_err(|error| {
            ActionOutcome::failed_product(format!("{method} {path} answered with no JSON: {error}"))
        })
    }

    async fn assign(
        &self,
        account_id: &str,
        module_id: &str,
        params: Value,
    ) -> Result<(), ActionOutcome> {
        self.send(
            reqwest::Method::PUT,
            &format!("/accounts/{account_id}/modules"),
            Some(json!({ "modules": [{ "module_id": module_id, "params": params }], "reason": REASON })),
        )
        .await
        .map(drop)
    }

    async fn clear(&self, account_id: &str) -> Result<(), ActionOutcome> {
        self.send(
            reqwest::Method::PUT,
            &format!("/accounts/{account_id}/modules"),
            Some(json!({ "modules": [], "reason": REASON })),
        )
        .await
        .map(drop)
    }

    async fn switch(&self, module_id: &str, enabled: bool) -> Result<(), ActionOutcome> {
        let route = if enabled { "enable" } else { "disable" };
        self.send(
            reqwest::Method::POST,
            &format!("/modules/{module_id}/{route}"),
            Some(json!({ "reason": REASON })),
        )
        .await
        .map(drop)
    }

    async fn instance(&self, module_id: &str) -> Result<Value, ActionOutcome> {
        let catalog = self.send(reqwest::Method::GET, "/modules", None).await?;
        catalog["modules"]
            .as_array()
            .and_then(|modules| modules.iter().find(|module| module["module_id"] == module_id))
            .cloned()
            .ok_or_else(|| {
                ActionOutcome::failed_setup(format!(
                    "the catalog has no instance {module_id}; qualification/stack/lib/modules.sh declares it"
                ))
            })
    }

    async fn history(&self, module_id: &str) -> Result<Vec<Value>, ActionOutcome> {
        let history = self
            .send(
                reqwest::Method::GET,
                &format!("/modules/{module_id}/params/history"),
                None,
            )
            .await?;
        Ok(history["entries"].as_array().cloned().unwrap_or_default())
    }
}

/// The first failure wins; a cleanup failure counts only when everything before it passed.
fn then_cleanup(outcome: ActionOutcome, cleanup: Result<(), ActionOutcome>) -> ActionOutcome {
    match (outcome, cleanup) {
        (ActionOutcome::Passed, Err(failure)) => failure,
        (outcome, _) => outcome,
    }
}

/// The fixture account, an authenticated client for it, and its proposal.
struct FixtureProposal {
    client: GuardianClient,
    account_id: miden_protocol::account::AccountId,
    nonce: u64,
    payload: miden_multisig_client::ProposalPayload,
}

impl FixtureProposal {
    async fn open(runner: &Runner, fixtures: &Fixtures) -> Result<Self, ActionOutcome> {
        let signer = fixtures.signer().map_err(|error| {
            ActionOutcome::failed_setup(format!("cannot build the fixture signer: {error}"))
        })?;
        let client = GuardianClient::connect(runner.endpoints.grpc.clone())
            .await
            .map_err(|error| {
                ActionOutcome::failed_setup(format!(
                    "cannot reach {}: {error}",
                    runner.endpoints.grpc
                ))
            })?
            .with_signer(Arc::new(signer));
        let account_id = miden_protocol::account::AccountId::from_hex(&fixtures.account_id)
            .map_err(|error| {
                ActionOutcome::failed_setup(format!("the fixture account id is malformed: {error}"))
            })?;
        let delta = fixtures.chained.first().ok_or_else(|| {
            ActionOutcome::failed_setup("the fixtures carry no threshold-only delta")
        })?;
        let payload = Fixtures::proposal_payload_for(&delta.payload).map_err(|error| {
            ActionOutcome::failed_setup(format!("building the fixture proposal payload: {error}"))
        })?;
        Ok(Self {
            client,
            account_id,
            nonce: delta.nonce,
            payload,
        })
    }

    async fn check(&mut self) -> Result<(), ClientError> {
        self.client
            .check_delta_proposal(&self.account_id, self.nonce, &self.payload)
            .await
            .map(drop)
    }

    async fn push(&mut self) -> Result<(), ClientError> {
        self.client
            .push_delta_proposal(&self.account_id, self.nonce, &self.payload)
            .await
            .map(drop)
    }
}

/// `Passed` when `result` is a denial by `module` for `reason_code`.
fn expect_denied(
    what: &str,
    result: Result<(), ClientError>,
    module: &str,
    reason_code: &str,
) -> ActionOutcome {
    match result {
        Ok(()) => {
            ActionOutcome::failed_product(format!("{what} was accepted while {module} refuses it"))
        }
        Err(error) => match error.module_refusal() {
            Some(ModuleRefusal::Denied {
                module: Some(denied_by),
                reason_code: code,
                ..
            }) if denied_by == module && code == reason_code => ActionOutcome::Passed,
            Some(refusal) => ActionOutcome::failed_product(format!(
                "{what} was refused as {refusal:?}, not by {module} for {reason_code}"
            )),
            None => ActionOutcome::failed_product(format!(
                "{what} failed without a module refusal: {error}"
            )),
        },
    }
}

fn expect_allowed(what: &str, result: Result<(), ClientError>) -> ActionOutcome {
    match result {
        Ok(()) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!("{what} was refused: {error}")),
    }
}

fn first_failure(outcomes: impl IntoIterator<Item = ActionOutcome>) -> ActionOutcome {
    outcomes
        .into_iter()
        .find(|outcome| !matches!(outcome, ActionOutcome::Passed))
        .unwrap_or(ActionOutcome::Passed)
}

/// An assigned in-process module refuses the fixture proposal, at the dry-run check and at
/// creation, with its id and reason code, and the same proposal passes once the assignment is
/// cleared.
pub async fn assert_refusal(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let operator = match Operator::login(runner, fixtures).await {
        Ok(operator) => operator,
        Err(outcome) => return outcome,
    };
    let mut proposal = match FixtureProposal::open(runner, fixtures).await {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    let account = fixtures.account_id.clone();

    if let Err(outcome) = operator.assign(&account, GATE, refuse_every_signer()).await {
        return outcome;
    }
    let refused = first_failure([
        expect_denied(
            "the proposal check",
            proposal.check().await,
            GATE,
            "signer_not_allowed",
        ),
        expect_denied(
            "the proposal",
            proposal.push().await,
            GATE,
            "signer_not_allowed",
        ),
    ]);
    let cleared = operator.clear(&account).await;
    let outcome = then_cleanup(refused, cleared);
    if !matches!(outcome, ActionOutcome::Passed) {
        return outcome;
    }
    expect_allowed(
        "the proposal check, once the module was unassigned,",
        proposal.check().await,
    )
}

/// The remote policy module the driver serves for the stack's `qual-remote` instance:
/// `signer-policy` over TLS, with the bearer token the Guardian reads from its secrets backend.
struct RemoteModule {
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl RemoteModule {
    async fn start() -> Result<Self, ActionOutcome> {
        let missing = |name: &str| {
            ActionOutcome::failed_setup(format!(
                "{name} is unset, but qualification/stack/run.sh always exports it"
            ))
        };
        let dir = std::path::PathBuf::from(
            std::env::var("QUAL_MODULE_TLS_DIR").map_err(|_| missing("QUAL_MODULE_TLS_DIR"))?,
        );
        let port: u16 = std::env::var("QUAL_MODULE_PORT")
            .map_err(|_| missing("QUAL_MODULE_PORT"))?
            .parse()
            .map_err(|error| ActionOutcome::failed_setup(format!("QUAL_MODULE_PORT: {error}")))?;
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|error| {
                ActionOutcome::failed_setup(format!("cannot read the module's {name}: {error}"))
            })
        };
        let identity = tonic::transport::Identity::from_pem(read("cert.pem")?, read("key.pem")?);
        let token = String::from_utf8_lossy(&read("token")?).trim().to_string();

        let service = guardian_module_sdk::serve::serve(SignerPolicy)
            .instance(REMOTE, token)
            .into_service();
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .map_err(|error| {
                ActionOutcome::failed_setup(format!("cannot listen on port {port}: {error}"))
            })?;
        let server = tonic::transport::Server::builder()
            .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
            .map_err(|error| ActionOutcome::failed_setup(format!("module TLS: {error}")))?
            .add_service(service);
        let incoming = tonic::transport::server::TcpIncoming::from(listener);
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let stopped = async {
                let _ = stopped.await;
            };
            if let Err(error) = server.serve_with_incoming_shutdown(incoming, stopped).await {
                eprintln!("the qualification policy module stopped: {error}");
            }
        });
        Ok(Self { shutdown, server })
    }

    /// Stops listening and closes the connection the Guardian holds open. Aborting the task
    /// is not enough: each accepted connection is served on a task of its own, which keeps
    /// answering after the listener is gone.
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let mut server = self.server;
        if tokio::time::timeout(std::time::Duration::from_secs(10), &mut server)
            .await
            .is_err()
        {
            server.abort();
        }
    }
}

/// A remote module decides for the account over TLS while it is up, its failure refuses
/// (`on_error = "deny"`) and names it while it is down, and disabling it is the way out.
///
/// The instance starts disabled in the catalog, because an enabled module that cannot be
/// reached stops the server from starting, and the module runs only for this action. So the
/// action always leaves it disabled and unassigned, whatever it concluded: left enabled, it
/// would stop the server coming back from the restart that follows the run.
pub async fn assert_unavailable(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let operator = match Operator::login(runner, fixtures).await {
        Ok(operator) => operator,
        Err(outcome) => return outcome,
    };
    let mut proposal = match FixtureProposal::open(runner, fixtures).await {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    let module = match RemoteModule::start().await {
        Ok(module) => module,
        Err(outcome) => return outcome,
    };
    let account = fixtures.account_id.clone();

    let (outcome, module) = remote_lifecycle(&operator, &mut proposal, &account, module).await;
    if let Some(module) = module {
        module.stop().await;
    }
    let disabled = operator.switch(REMOTE, false).await;
    let cleared = operator.clear(&account).await;
    then_cleanup(then_cleanup(outcome, disabled), cleared)
}

async fn remote_lifecycle(
    operator: &Operator<'_>,
    proposal: &mut FixtureProposal,
    account: &str,
    module: RemoteModule,
) -> (ActionOutcome, Option<RemoteModule>) {
    if let Err(outcome) = operator.switch(REMOTE, true).await {
        return (outcome, Some(module));
    }
    if let Err(outcome) = operator
        .assign(account, REMOTE, refuse_every_signer())
        .await
    {
        return (outcome, Some(module));
    }
    let decided = expect_denied(
        "the proposal check, with the remote module up,",
        proposal.check().await,
        REMOTE,
        "signer_not_allowed",
    );
    if !matches!(decided, ActionOutcome::Passed) {
        return (decided, Some(module));
    }

    module.stop().await;
    let down = match proposal.check().await {
        Ok(()) => ActionOutcome::failed_product(
            "the proposal check passed while the fail-closed remote module was down".to_string(),
        ),
        Err(error) => match error.module_refusal() {
            Some(ModuleRefusal::Unavailable {
                module: Some(unavailable),
            }) if unavailable == REMOTE => ActionOutcome::Passed,
            Some(refusal) => ActionOutcome::failed_product(format!(
                "with the remote module down, the check was refused as {refusal:?}, not as {REMOTE} unavailable"
            )),
            None => ActionOutcome::failed_product(format!(
                "with the remote module down, the check failed without a module refusal: {error}"
            )),
        },
    };
    if !matches!(down, ActionOutcome::Passed) {
        return (down, None);
    }

    if let Err(outcome) = operator.switch(REMOTE, false).await {
        return (outcome, None);
    }
    (
        expect_allowed(
            "the proposal check, with the down module disabled,",
            proposal.check().await,
        ),
        None,
    )
}

/// An operator disables a module that refuses, the account is let through at once, and the
/// stored switch survives a restart even though the catalog still says enabled.
///
/// Two passes, as `restart-durability` has: the first disables and leaves it disabled, the
/// pass after the restart finds it disabled, with the disable in the params history, and
/// enables it again.
pub async fn assert_break_glass(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let operator = match Operator::login(runner, fixtures).await {
        Ok(operator) => operator,
        Err(outcome) => return outcome,
    };
    if runner.post_restart && !runner.upgrade_target {
        return assert_switch_survived(&operator).await;
    }

    let mut proposal = match FixtureProposal::open(runner, fixtures).await {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    let account = fixtures.account_id.clone();
    if let Err(outcome) = operator.switch(SWITCH, true).await {
        return outcome;
    }
    if let Err(outcome) = operator
        .assign(&account, SWITCH, refuse_every_signer())
        .await
    {
        return outcome;
    }
    let outcome = first_failure([
        expect_denied(
            "the proposal check",
            proposal.check().await,
            SWITCH,
            "signer_not_allowed",
        ),
        match operator.switch(SWITCH, false).await {
            Ok(()) => expect_allowed(
                "the proposal check, with the module disabled,",
                proposal.check().await,
            ),
            Err(outcome) => outcome,
        },
    ]);
    then_cleanup(outcome, operator.clear(&account).await)
}

async fn assert_switch_survived(operator: &Operator<'_>) -> ActionOutcome {
    let instance = match operator.instance(SWITCH).await {
        Ok(instance) => instance,
        Err(outcome) => return outcome,
    };
    if instance["enabled"] != json!(false) || instance["catalog_enabled"] != json!(true) {
        return ActionOutcome::failed_product(format!(
            "after the restart {SWITCH} reads enabled = {} (catalog {}); the operator's disable \
             should have survived it",
            instance["enabled"], instance["catalog_enabled"]
        ));
    }
    let history = match operator.history(SWITCH).await {
        Ok(history) => history,
        Err(outcome) => return outcome,
    };
    if !history.iter().any(|entry| entry["reason"] == REASON) {
        return ActionOutcome::failed_product(format!(
            "the params history of {SWITCH} does not record the disable: {history:?}"
        ));
    }
    match operator.switch(SWITCH, true).await {
        Ok(()) => match operator.instance(SWITCH).await {
            Ok(instance) if instance["enabled"] == json!(true) => ActionOutcome::Passed,
            Ok(instance) => {
                ActionOutcome::failed_product(format!("enabling {SWITCH} again left it {instance}"))
            }
            Err(outcome) => outcome,
        },
        Err(outcome) => outcome,
    }
}

/// Assigns the gate to the scenario's account with a cap of exactly what `asset-send` sends,
/// and reads it back through the account's own policy listing.
pub async fn assign_cap(runner: &Runner) -> ActionOutcome {
    assign_to_funded_account(runner, CAP, |faucet| {
        let mut limit = serde_json::Map::new();
        limit.insert(faucet.to_hex(), json!(super::live::P2ID_AMOUNT.to_string()));
        json!({ "max_per_transaction": limit })
    })
    .await
}

/// Assigns `qual-recipient` to the scenario's account with a denylist naming the funding
/// faucet, an account the scenario never pays, and reads it back through the account's own
/// policy listing. `deny_unknown` is left unset, so it is on.
pub async fn assign_recipient_denylist(runner: &Runner) -> ActionOutcome {
    assign_to_funded_account(
        runner,
        RECIPIENT,
        |faucet| json!({ "denylist": [faucet.to_hex()] }),
    )
    .await
}

/// Assigns `module` to the scenario's account with the params `params` builds from the funding
/// faucet, and checks the account's own policy listing shows them.
async fn assign_to_funded_account(
    runner: &Runner,
    module: &str,
    params: impl FnOnce(miden_protocol::account::AccountId) -> Value,
) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(faucet) = session.faucet else {
        return ActionOutcome::failed_setup(
            "the account was never funded, so there is no asset to build the params from",
        );
    };
    let operator = match Operator::login(runner, fixtures).await {
        Ok(operator) => operator,
        Err(outcome) => return outcome,
    };
    let params = params(faucet);
    if let Err(outcome) = operator
        .assign(&session.account_id.to_string(), module, params.clone())
        .await
    {
        return outcome;
    }

    let policies = match session.clients[0].get_policies().await {
        Ok(policies) => policies,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing the account's policies failed: {error}"
            ));
        }
    };
    match policies.iter().find(|policy| policy.module_id == module) {
        Some(policy)
            if [&policy.params, &policy.account_layer]
                .iter()
                .any(|listed| listed.as_ref() == Some(&params)) =>
        {
            ActionOutcome::Passed
        }
        Some(policy) => ActionOutcome::failed_product(format!(
            "the account's policies list {module} without the params it was assigned: {policy:?}"
        )),
        None => ActionOutcome::failed_product(format!(
            "the account's policies do not list the {module} it was assigned: {policies:?}"
        )),
    }
}

/// One unit over the cap is refused by the dry-run check and at creation, naming the module and
/// `amount_cap_exceeded`; the capped amount itself passes the check. The transaction also pays
/// the network fee, so the capped amount passing is what shows the fee note is not counted as a
/// payment.
pub async fn assert_cap_refuses(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(faucet), Some(treasury)) = (session.faucet, session.treasury) else {
        return ActionOutcome::failed_setup(
            "the account was never funded, so it holds nothing to send",
        );
    };
    let amount = super::live::P2ID_AMOUNT;
    let send = |amount| TransactionType::transfer(treasury, faucet, amount);
    let client = &mut session.clients[0];

    let over_check = settled!(client.check_transaction(send(amount + 1)));
    let over_check = expect_module_denied(
        "the check of a payment over the cap",
        over_check.map(drop),
        CAP,
        "amount_cap_exceeded",
    );
    if !matches!(over_check, ActionOutcome::Passed) {
        return over_check;
    }
    let over = settled!(client.propose_transaction(send(amount + 1)));
    let over = expect_module_denied(
        "a payment over the cap",
        over.map(drop),
        CAP,
        "amount_cap_exceeded",
    );
    if !matches!(over, ActionOutcome::Passed) {
        return over;
    }
    match settled!(client.check_transaction(send(amount))) {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "the check of a payment of exactly the cap was refused, so the fee may count as a \
             payment: {error}"
        )),
    }
}

/// A payment to the blocked recipient is refused by the dry-run check and at creation, naming
/// the module and `recipient_blocked`; a payment to the treasury, which the denylist does not
/// name, passes the check. That transaction also carries the fee note, which has no recipient
/// a denylist can check, so it passing shows the fee note is not read as a payment.
pub async fn assert_recipient_refuses(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(faucet), Some(treasury)) = (session.faucet, session.treasury) else {
        return ActionOutcome::failed_setup(
            "the account was never funded, so it holds nothing to send",
        );
    };
    let amount = super::live::P2ID_AMOUNT;
    let client = &mut session.clients[0];

    let blocked_check =
        settled!(client.check_transaction(TransactionType::transfer(faucet, faucet, amount)));
    let blocked_check = expect_module_denied(
        "the check of a payment to the blocked recipient",
        blocked_check.map(drop),
        RECIPIENT,
        "recipient_blocked",
    );
    if !matches!(blocked_check, ActionOutcome::Passed) {
        return blocked_check;
    }
    let blocked =
        settled!(client.propose_transaction(TransactionType::transfer(faucet, faucet, amount)));
    let blocked = expect_module_denied(
        "a payment to the blocked recipient",
        blocked.map(drop),
        RECIPIENT,
        "recipient_blocked",
    );
    if !matches!(blocked, ActionOutcome::Passed) {
        return blocked;
    }
    match settled!(client.check_transaction(TransactionType::transfer(treasury, faucet, amount))) {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "the check of a payment to a recipient the denylist does not name was refused, so \
             the fee note may be read as a payment to an unknown recipient: {error}"
        )),
    }
}

/// A multisig SDK call refused by `expected_module` for `expected_reason`.
fn expect_module_denied(
    what: &str,
    result: Result<(), MultisigError>,
    expected_module: &str,
    expected_reason: &str,
) -> ActionOutcome {
    match result {
        Ok(()) => ActionOutcome::failed_product(format!("{what} was accepted")),
        Err(MultisigError::ModuleDenied {
            module: Some(module),
            reason_code,
            ..
        }) if module == expected_module && reason_code == expected_reason => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "{what} was refused, but not by {expected_module} for {expected_reason}: {error}"
        )),
    }
}
