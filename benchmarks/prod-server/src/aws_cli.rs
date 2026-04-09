use anyhow::{Context, Result, bail};
use std::env;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

pub async fn run_aws(args: &[&str], profile: Option<&str>, region: Option<&str>) -> Result<String> {
    let output = execute_aws(args, profile, region).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("aws command failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn run_aws_interactive(
    args: &[&str],
    profile: Option<&str>,
    region: Option<&str>,
) -> Result<String> {
    let output = execute_aws(args, profile, region).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("aws command failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!("{stdout}{stderr}").trim().to_string())
}

async fn execute_aws(
    args: &[&str],
    profile: Option<&str>,
    region: Option<&str>,
) -> Result<std::process::Output> {
    let mut command = Command::new(resolve_aws_binary());
    command.args(args);
    command.env("PATH", augmented_path());
    if let Some(profile) = profile {
        command.env("AWS_PROFILE", profile);
    }
    if let Some(region) = region {
        command.env("AWS_REGION", region);
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    command.output().await.context("failed to execute aws CLI")
}

pub fn assert_session_manager_plugin() -> Result<()> {
    if session_manager_plugin_available() {
        return Ok(());
    }
    bail!("session-manager-plugin is required for ecs-exec cleanup preflight");
}

pub fn resolve_aws_binary() -> String {
    for candidate in ["/opt/homebrew/bin/aws", "/usr/local/bin/aws", "aws"] {
        if candidate == "aws" || Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "aws".to_string()
}

fn session_manager_plugin_available() -> bool {
    [
        "/opt/homebrew/bin/session-manager-plugin",
        "/usr/local/bin/session-manager-plugin",
    ]
    .iter()
    .any(|candidate| Path::new(candidate).exists())
}

fn augmented_path() -> String {
    let mut segments = vec![
        "/usr/local/bin".to_string(),
        "/opt/homebrew/bin".to_string(),
    ];
    if let Some(existing_path) = env::var_os("PATH") {
        segments.extend(env::split_paths(&existing_path).map(|path| path.display().to_string()));
    }
    segments.join(":")
}

#[cfg(test)]
mod tests {
    use super::augmented_path;

    #[test]
    fn augmented_path_should_include_known_binary_locations() {
        let path = augmented_path();
        let segments = path.split(':').collect::<Vec<_>>();

        assert!(segments.contains(&"/usr/local/bin"));
        assert!(segments.contains(&"/opt/homebrew/bin"));
    }
}
