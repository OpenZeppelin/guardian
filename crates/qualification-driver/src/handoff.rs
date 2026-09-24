use std::path::Path;

use anyhow::{Context, anyhow};
use miden_client::rpc::Endpoint;
use miden_multisig_client::MultisigClient;
use miden_protocol::account::AccountId;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::manifest::NetworkName;

/// Cross-SDK handoff moves one cosigner's key between the two drivers.
///
/// The key is written as a serialized `AuthSecretKey`, whose leading byte names
/// the scheme, so neither side has to agree on anything beyond "these are the
/// bytes the SDK produces". It travels through a file rather than an argument
/// so it stays out of the process list.
pub fn write_key(path: &Path, key: &AuthSecretKey) -> anyhow::Result<()> {
    std::fs::write(path, hex::encode(key.to_bytes()))
        .with_context(|| format!("cannot write the handoff key to {}", path.display()))?;
    Ok(())
}

pub fn read_key(path: &Path) -> anyhow::Result<AuthSecretKey> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the handoff key from {}", path.display()))?;
    // The TypeScript side writes 0x-prefixed hex; the Rust side does not.
    let bytes =
        hex::decode(raw.trim().trim_start_matches("0x")).context("the handoff key is not hex")?;
    AuthSecretKey::read_from_bytes(&bytes)
        .map_err(|error| anyhow!("the handoff key is not a serialized AuthSecretKey: {error}"))
}

fn endpoint(network: NetworkName) -> Endpoint {
    match network {
        NetworkName::Devnet => Endpoint::devnet(),
        NetworkName::Testnet => Endpoint::testnet(),
    }
}

/// Signs one proposal on an account this process has never seen, using only a
/// handed-over key. Loading the account from GUARDIAN rather than from a shared
/// store is the point: it is what a second party would actually have.
pub async fn cosign(
    network: NetworkName,
    guardian_endpoint: String,
    account_id: &str,
    proposal_id: &str,
    key_path: &Path,
    account_dir: &Path,
) -> anyhow::Result<()> {
    let key = read_key(key_path)?;
    let account_id = AccountId::from_hex(account_id)
        .map_err(|error| anyhow!("account `{account_id}` is malformed: {error}"))?;

    std::fs::create_dir_all(account_dir)?;
    let builder = MultisigClient::builder()
        .miden_endpoint(endpoint(network))
        .guardian_endpoint(guardian_endpoint)
        .account_dir(account_dir);

    let builder = match key {
        AuthSecretKey::Falcon512Poseidon2(secret) => builder.with_secret_key(secret),
        AuthSecretKey::EcdsaK256Keccak(secret) => builder.with_ecdsa_secret_key(secret),
        other => anyhow::bail!("the handoff key uses an unsupported scheme: {other:?}"),
    };

    let mut client = builder
        .build()
        .await
        .map_err(|error| anyhow!("cannot build the cosigning client: {error}"))?;
    client
        .reset_miden_client()
        .await
        .map_err(|error| anyhow!("cannot reset the cosigning client: {error}"))?;
    client
        .pull_account(account_id)
        .await
        .map_err(|error| anyhow!("cannot load {account_id} from GUARDIAN: {error}"))?;
    client
        .sync()
        .await
        .map_err(|error| anyhow!("cannot sync before signing: {error}"))?;
    client
        .sign_proposal(proposal_id)
        .await
        .map_err(|error| anyhow!("cannot sign {proposal_id}: {error}"))?;

    Ok(())
}

pub struct TypescriptCosign {
    pub network: NetworkName,
    pub guardian_http_endpoint: String,
    pub account_id: String,
    pub proposal_id: String,
    pub scheme: crate::manifest::Scheme,
    pub key_file: std::path::PathBuf,
}

/// Signs a proposal in the TypeScript driver's own process.
///
/// A handoff scenario is only evidence of cross-SDK compatibility if the
/// signature is produced by the other SDK, so this crosses a process boundary
/// rather than reimplementing TypeScript's signing here. The TypeScript entry
/// point runs under vitest because that is what carries the module aliasing and
/// WASM initialization the package needs in Node.
pub async fn cosign_with_typescript(request: TypescriptCosign) -> anyhow::Result<()> {
    let scheme = match request.scheme {
        crate::manifest::Scheme::Falcon => "falcon",
        crate::manifest::Scheme::Ecdsa => "ecdsa",
        other => anyhow::bail!("{other:?} cannot be handed to the TypeScript driver"),
    };

    let repo_root = std::env::var("QUAL_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
    let package = Path::new(&repo_root).join("packages/miden-multisig-client");

    let output = tokio::process::Command::new("npx")
        .arg("vitest")
        .arg("run")
        .arg("--config")
        .arg("vitest.cosign.config.ts")
        .current_dir(&package)
        .env("QUAL_COSIGN_ACCOUNT_ID", &request.account_id)
        .env("QUAL_COSIGN_PROPOSAL_ID", &request.proposal_id)
        .env("QUAL_COSIGN_SCHEME", scheme)
        .env("QUAL_COSIGN_KEY_FILE", &request.key_file)
        .env("QUAL_HTTP_ENDPOINT", &request.guardian_http_endpoint)
        .env("QUAL_NETWORK", request.network.as_str())
        .env(
            "QUAL_MIDEN_RPC_ENDPOINT",
            std::env::var("QUAL_MIDEN_RPC_ENDPOINT")
                .unwrap_or_else(|_| format!("https://rpc.{}.miden.io", request.network.as_str())),
        )
        .output()
        .await
        .context("cannot start the TypeScript cosigner")?;

    if !output.status.success() {
        // Both streams, labelled. Vitest reports a failing assertion on stderr,
        // so stdout alone left the most useful half of a cross-SDK failure out
        // of the error the scenario reports.
        //
        // Capped and swept first. This is the one place the driver puts another
        // process's output into a message: the rest of its errors are text it
        // composed itself, so neither a size bound nor a sweep was needed
        // before. A vitest run can print megabytes, and this string becomes a
        // scenario `reason` in a retained artifact and a line in a CI log,
        // which the shell-side redaction covers only for the artifact.
        anyhow::bail!(
            "the TypeScript cosigner exited with {}\nstdout: {}\nstderr: {}",
            output.status,
            captured(&output.stdout),
            captured(&output.stderr)
        );
    }
    Ok(())
}

/// A subprocess stream, made safe to put in an error message.
///
/// Bounded, because the producer is a test runner that can print megabytes, and
/// swept for the one secret the driver knows it handed down: the child inherits
/// this process's environment, so `QUAL_TREASURY_KEY` is in scope for it, and a
/// crash dump that echoed the environment would otherwise reach a CI log
/// unredacted.
fn captured(stream: &[u8]) -> String {
    redacted_and_bounded(stream, std::env::var("QUAL_TREASURY_KEY").ok().as_deref())
}

/// Redacts before it bounds. Cutting first can split the secret across the
/// limit, and the fragment left on the kept side no longer matches the whole
/// value, so it survives both this replacement and the post-run scan, which
/// also looks only for the whole value.
fn redacted_and_bounded(stream: &[u8], secret: Option<&str>) -> String {
    const LIMIT: usize = 4096;

    let mut text = String::from_utf8_lossy(stream).into_owned();
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "[redacted]");
    }
    match text.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}… [truncated at {LIMIT} characters]", &text[..cut]),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{captured, redacted_and_bounded};

    #[test]
    fn short_output_is_passed_through() {
        assert_eq!(captured(b"boom"), "boom");
    }

    #[test]
    fn long_output_is_bounded_and_says_so() {
        let kept = captured(&vec![b'x'; 10_000]);
        assert!(kept.len() < 5_000, "kept {} characters", kept.len());
        assert!(kept.ends_with("[truncated at 4096 characters]"));
    }

    /// The child inherits this process's environment, so the one secret the
    /// driver knows about must not survive into an error message.
    #[test]
    fn the_treasury_key_does_not_survive() {
        // SAFETY: single-threaded test, and the variable is restored below.
        let previous = std::env::var("QUAL_TREASURY_KEY").ok();
        unsafe { std::env::set_var("QUAL_TREASURY_KEY", "deadbeefsecret") };
        let kept = captured(b"failed with key deadbeefsecret in scope");
        assert!(!kept.contains("deadbeefsecret"));
        assert!(kept.contains("[redacted]"));
        match previous {
            Some(value) => unsafe { std::env::set_var("QUAL_TREASURY_KEY", value) },
            None => unsafe { std::env::remove_var("QUAL_TREASURY_KEY") },
        }
    }

    /// A Falcon key is 2562 hex characters, so one printed near the limit
    /// straddles it. Truncating first kept its head, which no whole-value
    /// match could then find.
    #[test]
    fn a_key_straddling_the_limit_leaves_no_fragment() {
        let secret = "ab".repeat(1281);
        let mut stream = "x".repeat(3000).into_bytes();
        stream.extend_from_slice(secret.as_bytes());
        let kept = redacted_and_bounded(&stream, Some(&secret));
        assert!(
            !kept.contains(&secret[..64]),
            "a fragment of the key survived"
        );
        assert!(kept.contains("[redacted]"));
    }
}
