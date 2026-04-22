use crate::aws_cli::{assert_session_manager_plugin, run_aws};
use crate::client_factory::{build_auth, connect};
use crate::config::RunConfig;
use anyhow::{Context, Result, bail};
use std::path::Path;

pub async fn run(profile: &Path) -> Result<()> {
    let config = RunConfig::load_from_path(profile)?;
    let guardian_endpoint = config.normalized_guardian_endpoint();
    let mut commitments = Vec::new();

    for scheme in config.active_schemes() {
        let auth = build_auth(scheme);
        let pubkey = auth.public_key_hex();
        if pubkey.trim().is_empty() {
            bail!("failed to build signer for {}", scheme.as_str());
        }
        let mut client = connect(&guardian_endpoint)
            .await
            .with_context(|| format!("failed to connect to {guardian_endpoint}"))?;
        let (commitment, _) = client
            .get_pubkey(Some(scheme.as_str()))
            .await
            .with_context(|| {
                format!(
                    "failed to fetch {} GUARDIAN pubkey during preflight",
                    scheme.as_str()
                )
            })?;
        commitments.push((scheme, commitment));
    }

    std::fs::create_dir_all(&config.artifacts_dir).with_context(|| {
        format!(
            "failed to create artifacts directory {}",
            config.artifacts_dir.display()
        )
    })?;

    if config.cleanup.enabled {
        assert_session_manager_plugin()?;
    }

    verify_aws_access(
        config.aws.profile.as_deref(),
        Some(config.aws.region.as_str()),
    )
    .await?;

    println!("profile={}", config.profile_name);
    println!("guardian_endpoint={guardian_endpoint}");
    for (scheme, commitment) in commitments {
        println!(
            "guardian_pubkey_commitment_{}={commitment}",
            scheme.as_str()
        );
    }
    println!("artifacts_dir={}", config.artifacts_dir.display());
    println!("active_schemes={}", config.active_schemes().len());
    println!("preflight=ok");

    Ok(())
}

async fn verify_aws_access(profile: Option<&str>, region: Option<&str>) -> Result<()> {
    run_aws(&["sts", "get-caller-identity"], profile, region)
        .await
        .map(|_| ())
        .context("aws auth preflight failed")
}
