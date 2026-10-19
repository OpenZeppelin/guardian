# Feature Specification: Guardian Modules, a Hook Pipeline for Pluggable Policy

**Feature Branch**: `251-guardian-modules`
**Created**: 2026-10-13
**Status**: Draft
**Design source**: [RFC 0002](../../../docs/rfcs/0002-guardian-modules.md), revision 19. The RFC holds the wire protocol, configuration format and code citations; this spec states what the feature must achieve and how that is verified. Where the two differ, the RFC is corrected first and this spec follows.
**Input**: Issue #251 ("Modularization: middleware architecture and pluggable modules"), first consumer #182 (policy evaluation), later consumer #252 (x402 facilitator).
**Planning note**: `plan.md` records the pre-prototype plan and its prototype findings; the gates (account view extraction effort, projection reuse, added latency, module author ergonomics, placement relative to PR #510) and their results are in `research.md`. `tasks.md` holds the P0 prototype tasks and the follow-on phases generated since. The quickstart walk-through and an end-to-end run against a local node remain open.

## Context *(why this feature exists)*

Every rule about whether the Guardian co-signs a transaction is compiled into the server today: signature checks, candidate-chain rules, the account pause flag. Operators who must screen counterparties against sanctions lists, cap spending, restrict which keys may control an account, or consult a risk vendor have nowhere to put that logic short of forking the server.

This feature adds a **hook pipeline**: at fixed points in an account's lifecycle the Guardian asks an ordered list of **modules** whether to proceed, and each answers allow or deny. A module is either compiled into the Guardian or runs as a separate service the operator deploys. Modules are declared in server configuration, assigned to accounts by operators, and opt-in.

Four properties are preserved deliberately:

- **A remote module can only withhold the Guardian's signature, never produce one.** With any set of remote modules, the Guardian signs a subset of what it would sign without them.
- **Users can always leave.** Switching an account away from the Guardian never depends on any module.
- **No modules, no change.** A Guardian with no modules configured behaves and performs as it does today.
- **Modules complement on-chain policy, they do not replace it.** A rule in the account's own code holds even if the Guardian is bypassed; modules exist for rules that need off-chain data or must change without an account upgrade.

## Delivery Slices

The feature ships in three slices, each usable on its own. The module protocol is fixed at the first slice so modules written against it need no change later.

| Slice | Delivers | User stories |
|---|---|---|
| **M2a: stateless gates** | The pipeline at proposal creation, transaction approval and account registration; configuration; per-account assignment; remote and first-party modules; failure handling; the reference policy module; errors in both SDKs | 1 to 6 |
| **M2b: stateful limits** | Commit notifications, in-flight transaction accounting, delivery recovery, so limits over time are exact | 7 |
| **M2c: operator-built modules** | Compiling an operator's own module into a custom Guardian build | 8 |

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Enforce a mandatory policy on every account (Priority: P1, M2a)

An operator must apply a rule to every account it serves, for example a per-transaction amount cap or an allowlist of recipients and assets. The operator declares an instance of the first-party policy module in the Guardian's configuration with scope "every account", deploys, and from then on the Guardian refuses to co-sign any transaction that breaks the rule, however the transaction reaches it.

**Why this priority**: This is the core promise of #251 and the first need of most deployments (compliance and caps). Without it no other story has value.

**Independent Test**: Configure one instance with a per-transaction cap, run a transfer below and above the cap through each path that reaches the Guardian's approval, and observe which are signed.

**Acceptance Scenarios**:

1. **Given** a mandatory cap of 1,000 units, **When** cosigners push a transfer of 500, **Then** the Guardian signs it as it does today.
2. **Given** the same cap, **When** cosigners push a transfer of 5,000 through a stored proposal, **Then** the Guardian refuses with a policy-denied error naming the module instance and a reason code, and nothing is signed or stored as a candidate.
3. **Given** the same cap, **When** cosigners sign a transfer of 5,000 offline and push it directly without any stored proposal, **Then** it is refused exactly as in scenario 2.
4. **Given** the same cap, **When** a Guardian-executed proposal (RFC 0001) would transfer 5,000, **Then** the execution ends with a typed policy-denied outcome and nothing is submitted.
5. **Given** a proposal labelled "change threshold" whose actual effect moves funds above the cap, **When** it is pushed, **Then** the decision follows the actual effect, not the label.
6. **Given** a Guardian with no modules configured, **When** any existing flow runs, **Then** behaviour and results are unchanged.
7. **Given** a cap of 1,000 on one asset, **When** a transaction sends that asset in two notes of 600 each, **Then** it is refused, because the cap applies to the total sent in the transaction.
8. **Given** the same cap, **When** a transaction consumes an incoming note of 1,000 of that asset and sends 1,500 in the same transaction, **Then** it is refused, because incoming amounts do not offset outgoing ones.
9. **Given** the same cap, **When** a transaction sends 1,500 of a different asset that has no cap, **Then** the cap does not apply to it.

---

### User Story 2 - Learn about a refusal before anyone signs (Priority: P1, M2a)

A cosigner proposes a transaction that policy will refuse. Instead of every cosigner signing it and the refusal arriving only at approval, the proposal is refused when it is created, with the reason, and it does not occupy one of the account's limited proposal slots.

**Why this priority**: Without it, policy turns into wasted signing rounds and proposals that clog the account's slots indefinitely, since stored proposals are not cleaned up until something else moves the account forward.

**Independent Test**: Subscribe the policy instance to the proposal-creation point, create an over-cap proposal, and verify it is refused, nothing is stored, and the slot count is unchanged.

**Acceptance Scenarios**:

1. **Given** an instance subscribed at proposal creation with a cap of 1,000, **When** a cosigner creates a 5,000 proposal, **Then** creation is refused with the policy-denied error and no proposal is stored.
2. **Given** an account at its proposal limit minus one, **When** a refused proposal is attempted, **Then** the account can still create a valid proposal afterwards.
3. **Given** a proposal that passes at creation, **When** the policy has turned against it by approval time, **Then** approval refuses it and the creation-time verdict has no effect on that outcome.
4. **Given** an operator who wants a check only at approval, **When** the instance does not list the proposal-creation point, **Then** creation is not evaluated by it.
5. **Given** an operator who wants different behaviour at creation (other parameters, timeout or failure mode), **When** it declares a separate instance for the creation point, **Then** each point runs only its own instances.

---

### User Story 3 - Control which keys may sign for an account (Priority: P1, M2a)

An operator requires that every signer of every account comes from a list of vetted keys, or that certain keys are never accepted. The rule must hold when an account is registered, when a signer is added or replaced, and for transactions signed offline, without the module author having to understand the account's internal storage layout.

**Why this priority**: Control over who can authorize an account is as central to custody compliance as control over where funds go, and it exercises every gate point at once, which validates the shared request shape.

**Independent Test**: Configure a signer allowlist on all three gate points, then register an account with an unlisted signer, propose adding an unlisted signer, and push an offline signer change; each must be refused.

**Acceptance Scenarios**:

1. **Given** a signer allowlist, **When** an account registers with a signer not on the list, **Then** registration is refused.
2. **Given** the same list, **When** a cosigner proposes adding or replacing a signer with an unlisted key, **Then** the proposal is refused at creation, and the same transaction pushed offline is refused at approval.
3. **Given** the same list in "added signers only" mode, **When** an account whose existing signers predate the list makes an unrelated payment, **Then** the payment is not blocked by the list.
4. **Given** the list in "all signers" mode, **When** any transaction leaves the account with a signer not on the list, **Then** it is refused.
5. **Given** an account whose authorization state the Guardian cannot interpret, **When** a signer rule would be evaluated, **Then** the evaluation counts as a module failure and the instance's failure mode applies; it never passes as "no signers".

---

### User Story 4 - Choose and observe failure behaviour (Priority: P1, M2a)

A module can be slow, down or broken. For each instance the operator decides whether such a failure refuses approvals (compliance) or lets them proceed with a warning (advisory scoring), and can see what is happening.

**Why this priority**: A fail-closed module is a dependency on the approval path for every account it covers. Operators must be able to choose that consciously and detect outages before users report them.

**Independent Test**: Run one fail-closed and one fail-open instance, stop each module, and observe approvals, the returned error, latency, and the exported health signals.

**Acceptance Scenarios**:

1. **Given** a fail-closed instance whose module is unreachable, **When** a transaction is pushed, **Then** it is refused with a retryable policy-unavailable error.
2. **Given** a fail-open instance whose module is unreachable, **When** a transaction is pushed, **Then** it proceeds, and the skip is logged and counted.
3. **Given** a module that keeps failing, **When** several consecutive calls fail, **Then** later requests stop waiting for its timeout and its failure mode applies at once, until a health check shows it is back.
4. **Given** several modules on one account, **When** their combined time would exceed the per-request evaluation budget, **Then** the remaining modules count as timed out and their failure modes apply.
5. **Given** a configuration whose mandatory modules' timeouts exceed the budget, **When** the server starts in production mode, **Then** it refuses to start.
6. **Given** any instance, **When** an operator inspects health and metrics, **Then** per-instance availability, decision latency, decisions by outcome and fail-open counts are visible.
7. **Given** an unavailable fail-open module followed by a module that denies, **When** a transaction is pushed, **Then** evaluation continues past the first module and the transaction is refused by the second.

---

### User Story 5 - Tune policy per account without a deploy (Priority: P2, M2a)

Most accounts share defaults, but some need different limits, and some need optional modules. Instances are registered in configuration; everything about their parameters is managed at runtime through the operator interface: the global rules every account shares (for example extending a global blocklist), extra restrictions for one account on top of a mandatory rule, and the parameters of optional modules per account. An account's additions to a mandatory rule only add restrictions, so the global rule always holds. Every change carries a reason, an audit record and a history entry.

**Why this priority**: Real deployments have exceptions, and a deploy for every per-account limit is impractical. It is P2 because a single mandatory policy (Story 1) already delivers value.

**Independent Test**: Extend a mandatory blocklist globally and add an extra blocked address for one account, verify which accounts each blocks, attempt to remove the mandatory module from an account, and read the params history.

**Acceptance Scenarios**:

1. **Given** a module declared as "default", **When** a new account registers, **Then** the module is copied into its assignment with the default parameters, and existing accounts are unaffected.
2. **Given** a mandatory and a default module that both evaluate registration, **When** a new account registers before it has any assignment, **Then** both are evaluated with their configuration parameters, and a denial by either leaves no records.
3. **Given** an account, **When** an operator assigns an optional module with parameters and a reason, **Then** the module applies to that account only, and the change is audited with the operator identity and reason.
4. **Given** a mandatory module, **When** an operator tries to remove it from an account, **Then** the request is refused.
5. **Given** an instance not marked as operator-disableable, **When** an operator tries to disable it, **Then** the request is refused; enabling a declared instance is always allowed.
6. **Given** an operator holding only the narrow disable permission, **When** it disables a disableable instance, **Then** the request succeeds; **When** it tries to enable an instance or change an assignment, **Then** the request is refused, since both require the general policy permission.
7. **Given** a paused account, **When** a cosigner reconfigures it and every module allows, **Then** the reconfiguration succeeds and the account stays paused; **When** a module denies, **Then** nothing changes.
8. **Given** a released account, **When** a cosigner re-onboards it and every module allows, **Then** it is reactivated with its stored assignment; **When** a module denies, **Then** it stays released and unchanged.
9. **Given** an invalid parameter value, **When** an operator assigns it, **Then** it is refused at assignment time, not at the next transaction.
10. **Given** an account that re-registers, **When** registration succeeds, **Then** its stored assignment is preserved and never reset to catalog defaults by a cosigner.
11. **Given** a mandatory blocklist of A and B, **When** an operator adds C globally, **Then** every account is checked against A, B and C, on every replica, without a deploy.
12. **Given** the same blocklist, **When** an operator adds D for one account only, **Then** that account is checked against A, B, C and D, and every other account only against A, B and C.
13. **Given** a mandatory cap of 10,000 and an account layer of 50,000 for one account, **When** that account sends 20,000, **Then** it is refused by the global layer: an account layer cannot loosen a mandatory rule. **When** the account needs a higher limit, **Then** the rule belongs in an optional (default) module whose parameters the account can replace.
14. **Given** a denial, **When** the client and the operator inspect it, **Then** it names the instance, the reason code and whether the global or the account layer denied.
15. **Given** any parameter change, **When** an operator reads the history, **Then** it shows the previous and new parameters, the operator, the reason and the time, and each signed transaction records the parameter version it was checked against.
16. **Given** an operator holding only the per-account policy permission, **When** it tries to change global parameters, **Then** the request is refused.

---

### User Story 6 - Plug in a third-party module without rebuilding the Guardian (Priority: P2, M2a)

A compliance vendor or an operator team writes a module in any language and runs it as its own service. The operator points an instance at it in configuration, keeps running the official Guardian image, and the module sees only what the protocol sends it, authenticated and over an encrypted channel.

**Why this priority**: Most external data (sanctions, risk) lives with vendors. Requiring a custom build for each would make modules impractical. P2 because first-party modules (Stories 1 to 3) work without it.

**Independent Test**: Run the reference policy as a separate service through the SDK's serving harness, configure it as a remote instance, and verify identical decisions to the in-process variant.

**Acceptance Scenarios**:

1. **Given** the reference policy run in process and as a remote service, **When** the conformance suite runs, **Then** both make identical decisions on every case.
2. **Given** a remote instance without authentication configured, **When** the server starts in production mode, **Then** it refuses to start.
3. **Given** one remote service backing two instances with separate credentials, **When** a request authenticated for one instance names the other, **Then** the service rejects it.
4. **Given** an enabled instance whose module does not support a hook it is configured for, speaks an unsupported protocol version or is unreachable, **When** the server starts in production mode, **Then** it refuses to start.
5. **Given** the same failure on an instance declared disabled, **When** the server starts in production mode, **Then** it starts, logs the failure and keeps the instance disabled; **When** an operator later enables it while the failure persists, **Then** enabling is refused.

---

### User Story 7 - Enforce limits over time exactly (Priority: P2, M2b)

An operator caps an account's daily volume, or limits how much each proposer can move per day. The limit must hold even when several transactions are signed in quick succession, when commit notifications are delayed, or when the module was briefly down.

**Why this priority**: Stateful limits are most of the #238 policy list and the first consumer #182. They need delivery machinery beyond M2a, so they form their own slice.

**Independent Test**: Configure a daily volume cap, push transactions concurrently while delaying notifications, and verify the cap is never exceeded in candidate mode. Exactness holds while the module is reachable and not awaiting resync; outside that, its failure mode decides (scenarios 3 and 4).

**Acceptance Scenarios**:

1. **Given** a daily cap of 1,000, **When** two 800 transfers are pushed before either commits, **Then** the second is refused.
2. **Given** a per-proposer daily cap, **When** a proposer's earlier transactions are signed but not yet reported to the module, **Then** they count against that proposer.
3. **Given** a fail-closed module that missed notifications beyond the retention window, **When** a transaction is pushed for the affected account, **Then** it is refused without the module being called until an operator explicitly resyncs it, accepting the lost history, and the module is told which account lost history.
4. **Given** the same situation for a fail-open module, **When** a transaction is pushed, **Then** it proceeds, the skip is logged and counted, and the limit is not enforced for that account until the resync.
5. **Given** a notification delivered twice, or delivered late by a replica that lost leadership, **When** the module processes it, **Then** its state is unchanged by the duplicate.
6. **Given** a module that has applied a notification before the Guardian records its acknowledgement, **When** the next evaluation lists that transaction as pending, **Then** the module counts it once, not twice.
7. **Given** a candidate queued before the first stateful module was deployed, **When** a transaction is pushed for that account, **Then** the stateful module is not called and its failure mode applies until that candidate is promoted or discarded; afterwards, and for accounts assigned a stateful module later, evaluation proceeds normally.
8. **Given** a transaction signed at 23:59 and committed at 00:01, **When** daily windows are computed, **Then** it belongs to the earlier day, both before and after notification.

---

### User Story 8 - Compile an operator-built module into the Guardian (Priority: P3, M2c)

An operator with a module it wrote or audited runs it inside the Guardian process for the lowest latency or for access to private state, accepting a custom build and full trust in that code.

**Why this priority**: A minority need; remote modules cover most cases without the trust and maintenance cost.

**Independent Test**: Build the published custom-build template with an example module, configure an instance of it, and verify it behaves like a first-party module.

**Acceptance Scenarios**:

1. **Given** the custom-build template of a release, **When** an operator adds a module crate and builds with the release's dependency lock, **Then** the resulting server registers the module and its instances work like first-party ones.
2. **Given** a configuration naming a module kind the build does not register, **When** the server starts, **Then** it refuses to start.

---

### Edge Cases

- **Leaving the Guardian**: an account switching to another Guardian is never blocked; a module may refuse the best-effort switch notice sent to the old Guardian, and the release is then detected from chain.
- **Paused or released accounts** are refused before any module sees their transactions or proposals; reconfiguring a paused account and re-onboarding a released one remain allowed and run the registration modules.
- **Proposer attribution**: when two cosigners create the same proposal, the first creator remains its proposer on every storage backend. When proposal lookup fails because of a storage fault, that is a module failure, never "no proposer".
- **Missing proposer at approval** (offline push, single-key account, proposal already removed): the proposer is empty; a rule that must hold treats it as unknown and refuses.
- **Forged metadata**: proposer-supplied labels and fields never change a decision; only the decoded effect does.
- **Oversized requests**: a request too large to send counts as a module failure without the call being made.
- **Rolling deploys**: replicas may briefly run different configurations; each signed transaction records which configuration approved it.
- **Removed instances**: an account assignment naming an instance no longer configured is kept, skipped, and reported, and becomes active again if the instance returns.
- **Over-budget assignments** after a deploy that raised timeouts are reported at startup without blocking it; the runtime budget enforces the limit.
- **First stateful module deployed while candidates are queued**: those candidates lack the data the module needs; the module's failure mode applies for that account until they leave the queue.
- **Execution requester**: a repeated execution request for an active attempt keeps the original requester as the submitter.
- **Filesystem backend** (development only) runs gate hooks but no stateful machinery; a configuration that needs it refuses to start there.

## Requirements *(mandatory)*

Slice tags show where each requirement is first delivered. Wire formats, field names and configuration keys are defined in RFC 0002.

### Pipeline and hooks

- **FR-001** [M2a]: The Guardian MUST evaluate the account's ordered module set at three gate points: transaction approval, proposal creation, and account registration (new and re-onboarding). Evaluation stops at the first denial.
- **FR-002** [M2a]: Transaction approval MUST be evaluated inside the single function that produces the Guardian's approval, so no caller (direct push, proposal-based push, Guardian execution) can obtain an approval without the pipeline.
- **FR-003** [M2a]: A denial at approval MUST produce no signature and store no candidate. A denial at proposal creation MUST store nothing and consume no proposal slot. A denied new registration MUST create no account records. A denied reconfiguration of an existing account MUST leave its stored state, metadata, pause and release state, and module assignment unchanged.
- **FR-004** [M2a]: Each gate point MUST run only the instances that list it; nothing is inherited between points. One instance MAY list several points.
- **FR-005** [M2a]: Account switching away from the Guardian MUST NOT depend on any module decision.
- **FR-006** [M2a]: An account with an empty module set MUST skip the pipeline entirely; a Guardian with no modules configured MUST behave as before this feature.
- **FR-007** [M2a]: At transaction approval and proposal creation, the existing pause and release checks MUST run before any module is called, so a paused or released account is refused without any module seeing its transaction. Registration MUST NOT gain a pause or release check: reconfiguring a paused account and re-onboarding a released one stay allowed as today, registration modules run for them, a successful reconfiguration preserves the pause state, and reactivation of a released account happens only if every module allows.
- **FR-008** [M2a]: Modules MUST be evaluated in a defined order: mandatory modules in configuration order, then the account's assigned modules in their stored order.

### Configuration and scopes

- **FR-010** [M2a]: Module instances MUST be declared only in server configuration loaded at startup; an operator session MUST NOT be able to add an instance or change its kind, endpoint, authentication, hooks, timeout or failure mode.
- **FR-011** [M2a]: Each instance MUST have a scope: mandatory for every account, default (copied into new accounts and removable per account), or assigned (only where an operator assigns it).
- **FR-012** [M2a]: Default modules MUST NOT be applied retroactively to existing accounts, and later changes to default parameters MUST NOT change parameters already copied into accounts.
- **FR-013** [M2a]: Instances MUST be registered only in configuration, which also seeds each instance's global parameters the first time it is seen; afterwards global parameters MUST be managed only through the operator interface, and a configuration change to them MUST be reported as drift, not applied. Each request MUST carry only the parameters of the layer being evaluated, never another account's.
- **FR-014** [M2a]: At startup the Guardian MUST verify that each instance supports its configured hooks, speaks a supported protocol version, and accepts its parameters. In production mode a failure of an enabled instance MUST stop startup; a disabled instance that fails is logged and stays disabled, so an unavailable module can be staged without blocking a deploy, and enabling it MUST run the same checks first and refuse on failure.
- **FR-015** [M2a]: Secret references MUST be allowed only in the configuration of in-process instances and MUST be rejected in remote-instance parameters, in per-account exceptions and through the operator interface. The Guardian resolves them when it creates the instance and supplies the resolved values to the in-process module at creation. Resolved values MUST never appear in any request sent to a module; an in-process module receives its parameters with the references unresolved.
- **FR-016** [M2a]: Each replica MUST expose a fingerprint of the configuration it loaded, and every signed transaction MUST record the fingerprint that approved it.
- **FR-017** [M2a]: The Guardian and each module MUST exchange their protocol versions and capabilities when the module is described. The Guardian MUST accept its current protocol version and, once a newer one exists, the previous one for one release, warning about the older one; it MUST refuse an enabled instance outside that range or one that requires a capability the Guardian lacks. Within a protocol version, changes MUST be additive only.
- **FR-018** [M2a]: A decision MUST state its effect explicitly, with decisions that predate the field read from their allow flag; an effect the Guardian does not recognise MUST count as a module failure, never as an allow.
- **FR-019** [M2a]: A module MUST be able to return its parameters in its current shape when validating them; the Guardian MUST store that shape for operator writes, and at startup and before enabling MUST store migrated global parameters with a history entry attributed to the module.

### Per-account administration

- **FR-020** [M2a]: Operators MUST be able to read an account's effective module set (each mandatory module's global parameters and the account's layer, and each optional module's parameters), replace its assigned and default modules with an ordered list and parameters, change any instance's global parameters, and set or remove an account's layer on a mandatory module, each with a required reason, recorded in the audit log with the operator identity.
- **FR-021** [M2a]: An account's layer on a mandatory module MUST be evaluated as a second pass after the global parameters, and the transaction MUST pass only if both allow, so a layer can only add restrictions. The operator interface MUST refuse removing a mandatory module from an account, parameters containing secret references, changes to parameters the configuration holds with secret references, and assignments or layers whose combined timeouts exceed the evaluation budget.
- **FR-022** [M2a]: Parameters MUST be validated by the module when they are written, each layer on its own.
- **FR-023** [M2a]: Operators MUST be able to enable any declared instance, under the general policy permission only, and disable only instances the configuration marks as disableable, under either the general policy permission or a dedicated narrow disable permission that grants nothing else; a disabled instance MUST keep receiving commit notifications.
- **FR-026** [M2a]: Every parameter write MUST append to a history (previous and new parameters, operator, reason, time); the parameter store MUST carry a version that increases with every write, and every signed transaction MUST record it next to the configuration fingerprint.
- **FR-027** [M2a]: Changing global parameters MUST require a separate permission from per-account policy changes.
- **FR-028** [M2a]: Each evaluation MUST use global parameters, enabled flags and the account's parameters from a single parameter version: a request MUST NOT combine values from configurations that were never committed together, even while operators write concurrently.
- **FR-029** [M2a]: A write derived from an earlier read (a module migrating stored parameters at startup or before enabling) MUST apply only if the stored value is still the one read, so it never overwrites a newer operator write.
- **FR-024** [M2a]: Operators MUST be able to list all instances with their runtime state, including differences between configuration and runtime switches and the circuit state.
- **FR-025** [M2a]: Re-registration of an existing account MUST preserve its stored assignment.

### What modules receive

- **FR-030** [M2a]: Every gate request MUST carry the account's state before and after the request as decoded by the Guardian (commitment, authorization scheme, signer set, default threshold, per-procedure thresholds), so one rule can be evaluated identically at every gate point. "Before" is absent only for a new registration.
- **FR-031** [M2a]: Extracting those views MUST fail with an error, treated as a module failure, for authorization state the Guardian does not support or cannot decode; it MUST NOT produce an empty signer set. Building "before" at re-onboarding MUST read the stored state.
- **FR-032** [M2a]: Transaction requests MUST carry a decoded, versioned projection of the transaction's effects (asset movements, notes, storage changes, nonce and expiration) so modules need no Miden decoder, plus the raw transaction summary for modules that accept coupling to the Guardian's Miden version.
- **FR-033** [M2a]: Every gate request MUST carry the verified identity of the cosigner whose authenticated request is being evaluated (the submitter). On the execution path this MUST be the authenticated requester that created the execution attempt, recorded durably with the attempt and preserved across repeated requests and restarts.
- **FR-034** [M2a]: Transaction requests MUST carry the verified identity of the proposal's creator (the proposer) when a matching stored proposal exists, and an empty value otherwise. Proposal lookup MUST distinguish absence from failure; a lookup failure is a module failure.
- **FR-035** [M2a]: The first creator of a proposal MUST remain its proposer on every storage backend. Proposal creation and signature updates MUST be separate operations: a duplicate creation keeps the stored proposal, while adding a cosigner signature updates it, on both backends.
- **FR-036** [M2a]: Proposer-supplied proposal metadata MUST be passed only as explicitly untrusted data, and no typed copy of proposer-asserted values (amounts, recipients, signers) may be provided.
- **FR-037** [M2a]: Each request MUST identify the instance being called, so one remote service can back several instances with separate state and credentials.
- **FR-038** [M2a]: Each request MUST be bounded in size; an oversized request counts as a module failure without the call being made.

### Decisions and errors

- **FR-040** [M2a]: A denial MUST carry a machine-readable reason code; a denial without a valid code is a module failure.
- **FR-041** [M2a]: Clients MUST receive a non-retryable policy-denied error and a retryable policy-unavailable error naming the instance. For an instance that denies with full reasons, the denial MUST name the operator-chosen instance identifier, the reason code and the layer (global or account) that denied; for an instance that denies generically, it MUST carry none of them (FR-056). The module's free-text message MUST NOT reach clients.
- **FR-042** [M2a]: Both errors MUST be returned on proposal creation and on approval, and on the execution path as typed execution outcomes.
- **FR-043** [M2a]: The Rust and TypeScript clients and both multisig SDKs MUST expose these errors as typed errors with identical retry classification.

### Failure handling

- **FR-050** [M2a]: Each instance MUST have a failure mode, refuse (default) or proceed, applied on timeout, transport error, malformed decision, unsupported protocol version, oversized request, view extraction or proposal lookup failure, open circuit, and (M2b) stale state, backlog over the cap or pending resync.
- **FR-051** [M2a]: Each call MUST be bounded by its own timeout and by a per-request evaluation budget; the budget MUST be checked at startup against mandatory and default modules and against the execution lease, and per account at assignment.
- **FR-052** [M2a]: Each replica MUST keep a circuit breaker per instance that stops calling a failing module and applies its failure mode immediately, and a periodic health check that reports availability and detects protocol changes.
- **FR-053** [M2a]: Evaluation MUST be sequential: a module is called only after every earlier module either allowed or failed with a failure mode that lets the transaction proceed. A denial, or a failure of a fail-closed module, stops evaluation.
- **FR-054** [M2a]: Module evaluation MUST be free of side effects, since the same transaction is evaluated at creation and at approval and clients retry; module state MUST change only from notifications (M2b). The conformance suite MUST evaluate the same request repeatedly and verify the module's state is unchanged.
- **FR-055** [M2a]: Each instance MUST declare what an account's cosigners may learn about it (nothing, a description, or the description and its effective params), defaulting to nothing; disclosing anything MUST require an operator-written description.
- **FR-056** [M2a]: An instance MUST be able to deny generically: the client learns that a policy refused and nothing about which module, layer or reason, while the operator's log keeps the full decision.
- **FR-057** [M2a]: A module MAY return a message for the requester; the Guardian MUST forward it only for instances that deny with full reasons, as plain text of at most 256 characters.
- **FR-058** [M2a]: The account's cosigners MUST be able to list the policies that apply to the account, filtered by disclosure, and to check a proposal without storing it or taking a slot; the check MUST be rate limited per account and signer, logged, skip instances that opt out (reported as an incomplete check without naming them), and be advisory only.

### Stateful limits

- **FR-060** [M2b]: The Guardian MUST notify subscribed modules of every committed transaction, at least once and in order per account, through records written atomically with the commit, with one failing account never delaying others.
- **FR-067** [M2b]: Once any instance subscribes to commit notifications, the data `pending` needs MUST be recorded with every transaction the Guardian signs, for every account, so a stateful module assigned later finds every queued transaction prepared. While a transaction signed without that data is queued for an account, every subscribed module's evaluation for that account MUST be treated as a module failure until the transaction is promoted or discarded.
- **FR-061** [M2b]: Every evaluation for a subscribed module MUST include the transactions signed for the account that the module has not yet acknowledged, each with its accounting time, account nonce, submitter and proposer, so limits count every signed transaction exactly once in candidate mode.
- **FR-062** [M2b]: Time windows MUST use one accounting clock, the time the Guardian signed the transaction, fixed before evaluation and repeated unchanged in notifications.
- **FR-063** [M2b]: Notifications MUST carry the account nonce so modules can discard duplicates and late deliveries; delivery after leadership changes MUST be fenced.
- **FR-064** [M2b]: Undelivered notifications MUST be retained for a configurable period; when history is dropped the affected module enters resync-required for that account. While it is set, the module is not called for that account and its failure mode applies: fail-closed instances refuse, fail-open instances let the transaction proceed with the skip logged and counted. An operator resyncs with a reason, audited; the module is told which account lost history, and the refusal lasts until the module has acknowledged that notice, so no evaluation and no later notification reaches the module before it.
- **FR-065** [M2b]: A module MAY declare a maximum notification lag; beyond it, and beyond a backlog cap, its failure mode applies.

### Proposal filtering

- **FR-070** [M2a]: Evaluation at proposal creation MUST use the same request shape as at approval, with "before" being the state the proposal is pinned to.
- **FR-071** [M2a]: Creation-time verdicts MUST NOT be treated as enforcement; open proposals MUST NOT be counted as in-flight transactions for limits.

### Extensibility

- **FR-080** [M2a]: First-party in-process modules MUST be included in the official image and usable by configuration alone, including the reference policy: recipient allowlist and denylist (with an option to refuse notes whose recipient cannot be determined), asset allowlist, a per-transaction amount cap per asset over the total amount sent in the transaction (not offset by amounts received, fee excluded), and signer allowlist and denylist with a choice between checking added signers and all signers.
- **FR-081** [M2a]: A published module SDK MUST provide the module interface, protocol types, a serving harness for remote modules, and a conformance suite that runs the same cases against in-process and remote modules.
- **FR-082** [M2c]: Operators MUST be able to register their own module kinds in a custom build from a published template that pins the release's dependency lock; the registration interface is versioned public API.
- **FR-083** [M2a]: Module kinds MUST NOT be loadable at runtime.

### Security

- **FR-090** [M2a]: Remote modules MUST be reached over encrypted, server-authenticated connections; in production mode each MUST present a per-instance credential, and mutual authentication MAY be configured.
- **FR-091** [M2a]: No Guardian signing key may be used for module authentication.
- **FR-092** [M2a]: The trust boundary documentation MUST state that external modules receive full transaction details, including proposals that never execute and proposer descriptions.

### Observability and documentation

- **FR-100** [M2a]: The Guardian MUST export per-instance decision latency by outcome, availability, circuit state, fail-open counts, disabled state, configuration drift and fingerprint, and the counts of orphaned and over-budget assignments; M2b adds notification lag, dropped events and the activation fence.
- **FR-101** [M2a]: Configuration, troubleshooting, the trust boundary and a module author guide MUST be documented; M2b adds that stateful limits are exact only in candidate mode; M2c adds the in-process author path.

### Key Entities

- **Module kind**: an implementation of the module interface, registered under a name when the server is built.
- **Module instance**: a configuration entry naming a kind, with scope, hooks, timeout, failure mode, credentials, default parameters and per-account exceptions.
- **Account assignment**: the ordered list of default and assigned instances for one account, with their parameters, written only through the operator interface.
- **Parameter layers**: an instance's global parameters (seeded from configuration, then operator-managed) and, on a mandatory instance, an account's layer that only adds restrictions; each write recorded in a versioned history.
- **Hook request**: what a module receives at a gate point: identity of the call, effective parameters, submitter, account views before and after, and either a transaction or a registration subject.
- **Account view**: the Guardian-decoded authorization state of an account: commitment, scheme, signers, default and per-procedure thresholds.
- **Proposal context**: the verified proposer and untrusted metadata of the matching stored proposal.
- **Decision**: allow, or deny with a reason code.
- **Commit event** [M2b]: a committed transaction with its projection, account nonce, accounting time, submitter and proposer.
- **Pending transaction** [M2b]: a signed but unacknowledged transaction included in evaluations.
- **Resync state** [M2b]: the per-module, per-account condition after dropped history, cleared only by an operator.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With no modules configured, every existing automated test and qualification scenario passes unchanged, and approval latency at the 99th percentile changes by no more than measurement noise.
- **SC-002**: In the acceptance suite, 100% of transactions that break a mandatory policy are refused on every path that reaches approval: proposal-based push, direct push of an offline-signed transaction, and Guardian execution.
- **SC-003**: 100% of proposals refused at creation leave no stored proposal and no change in the account's proposal slot count.
- **SC-004**: An operator can enable the reference policy on all accounts by editing configuration and deploying the official image, with no build step, following the documentation in under 30 minutes.
- **SC-005**: The reference policy produces identical decisions in process and as a remote service on 100% of conformance cases.
- **SC-006**: One in-process module adds no more than 10 ms to approval latency at the 99th percentile; the measured overhead for an in-process and a remote module is recorded in the plan and the default budget revisited against it.
- **SC-007**: When a fail-closed module's circuit is open, refusals are returned without waiting for the module's timeout; when a fail-open module is down, approvals proceed with no more added latency than its timeout until the circuit opens.
- **SC-008** [M2b]: Under concurrent pushes, delayed notifications, duplicate deliveries, acknowledgements recorded after the module applied a notification, and a leadership change, a daily volume cap is never exceeded in candidate mode across the conformance and qualification runs, for a module that is reachable and not awaiting resync.
- **SC-009**: 100% of operator changes to module assignments, global parameters, account layers, enablement and resync carry a reason, an audit record with the operator identity and, for parameters, a history entry.
- **SC-012**: An operator can extend a global blocklist, or add a blocked address for one account, through the operator interface in under one minute, and the change applies on every replica to the next evaluation, without a deploy.
- **SC-010**: The Rust and TypeScript SDKs classify every module error identically, verified by matching test cases in both.
- **SC-011**: A module author can build a working remote module from the SDK and example in under one day without reading Guardian server code.

## Assumptions

- The official server runs in candidate mode; stateful limits are exact only there, and the optimistic mode's limitation is documented rather than solved.
- Stateful machinery (notifications, in-flight accounting, resync) requires the Postgres backend; the filesystem backend is for development and runs gate hooks only.
- Guardian execution (RFC 0001, PR #510) lands before or with M2a; until it does, the execution-path requirements apply when it merges.
- The Miden protocol line is 0.17; projections and account views are re-derived on protocol upgrades by the Guardian, not by modules.
- Proposer metadata follows the multisig SDKs' schema and may change between SDK releases.
- No operator dashboard page is built; the operator interface and its client library are in scope.
- Default timeouts and budget follow RFC 0002 (budget 1,000 ms, per-instance maximum 1,000 ms) until prototype measurements revise them.

## Out of Scope

- The HTTP module kind (added later if operators ask).
- Decisions that require additional approvals, and external approvers.
- Account and proposal lifecycle notifications (released, registered, policy changed, proposal closed, proposal signed).
- A current verdict on proposal signing and in listings; proposal cancel and expiry.
- EVM accounts, TEE deployment tiers, WebAssembly modules, adding instances without a deploy, account self-service configuration, bulk assignment, a registry of standard reason codes, and side-effecting modules such as the x402 facilitator.
