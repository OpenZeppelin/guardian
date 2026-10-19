---- MODULE PushGateThreshold ----
(***************************************************************************)
(* The signature count Guardian's multisig push gate requires before it   *)
(* acknowledges a delta, compared with the count the chain requires for   *)
(* the same transaction.                                                  *)
(*                                                                        *)
(* Gate: crates/server/src/services/multisig_admission.rs                 *)
(*   MultisigAccount::signature_floor: the default threshold, or the     *)
(*   lowest non-zero per-procedure override if it is lower. The gate      *)
(*   reads nothing of the transaction to set the count.                   *)
(* Chain: miden-standards (pinned in Cargo.lock)                          *)
(*   asm/standards/auth/multisig.masm compute_transaction_threshold,     *)
(*   called from auth_tx with get_initial_threshold_and_num_approvers.    *)
(*                                                                        *)
(* Both sides read thresholds and overrides from the state the            *)
(* transaction executes on: the chain through get_initial_item /          *)
(* get_initial_map_item, the gate through the queue tail (push_delta      *)
(* passes tail.state_json). So one config serves both.                    *)
(*                                                                        *)
(* Every state of this spec is one (transaction, account config) pair;    *)
(* there are no steps. TLC enumerates them all and checks the invariants  *)
(* on each.                                                               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    NumApprovers,     \* thresholds and overrides range over 1..NumApprovers
    MaxCalls,         \* bound on how many distinct account procedures one transaction calls
    CustomComponent   \* the account carries a procedure outside BasicWallet + AuthGuardedMultisig

\* Non-auth account procedures of a Guardian multisig account
\* (guarded_multisig.masm and basic_wallet.masm `pub use` exports). Calls
\* that leave no trace in the summary are included: the gate does not read
\* the summary, so they are ordinary calls here.
Procs == {"receive_asset", "create_note", "move_asset_to_note",
          "update_signers_and_threshold", "set_procedure_threshold",
          "update_guardian_public_key", "get_threshold_and_num_approvers",
          "get_signer_at", "is_signer"}
         \cup (IF CustomComponent THEN {"custom"} ELSE {})

\* Stands for every override on a procedure the transaction does not call.
\* The chain never reads those; the gate's floor does. One representative
\* suffices because only their minimum matters to the floor.
Other == "other_procedure"

Max(S) == CHOOSE m \in S : \A x \in S : x <= m
Min(S) == CHOOSE m \in S : \A x \in S : m <= x

VARIABLES calls, dflt, ov
vars == <<calls, dflt, ov>>

\* An override of 0 is "no override" on both sides (signature_floor's
\* filter, the MASM `dup eq.0`).
SetOverrides == {ov[r] : r \in {r2 \in DOMAIN ov : ov[r2] # 0}}

\* MultisigAccount::signature_floor
GateRequired == Min({dflt} \cup SetOverrides)

PerProc(p) == IF ov[p] # 0 THEN ov[p] ELSE dflt

\* compute_transaction_threshold: max over called non-auth procedures of the
\* override or the default; the default when none was called.
ChainRequired == IF calls = {} THEN dflt ELSE Max({PerProc(p) : p \in calls})

Init ==
    /\ calls \in {c \in SUBSET Procs : Cardinality(c) <= MaxCalls}
    /\ dflt \in 1..NumApprovers
    /\ ov \in [calls \cup {Other} -> 0..NumApprovers]

Next == UNCHANGED vars

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Properties                                                             *)
(***************************************************************************)

\* Guardian never asks for more approvals than the chain will. If it does,
\* a transaction the chain would accept cannot be executed through Guardian.
NeverOverAsks == GateRequired <= ChainRequired

\* On an account with no override below the default, Guardian asks for the
\* default threshold. If not, fewer cosigners than the default can hold the
\* candidate slot with a delta the chain will refuse (issue #524).
DefaultWithoutLowOverride ==
    (\A r \in DOMAIN ov : ov[r] = 0 \/ ov[r] >= dflt) => GateRequired = dflt

====
