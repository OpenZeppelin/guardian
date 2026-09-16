use serde::Deserialize;

use super::{ActionOutcome, Runner};

#[derive(Debug, Deserialize)]
struct StatusResponse {
    status: String,
    version: String,
    git_commit: String,
    environment: String,
}

/// The build context excludes version-control metadata, so an image built
/// without the commit passed in reports `unknown` and this assertion would
/// compare nothing.
pub async fn assert_identity(runner: &Runner) -> ActionOutcome {
    let url = format!("{}/status", runner.endpoints.http);
    let response = match runner.http.get(&url).send().await {
        Ok(response) => response,
        Err(error) => {
            return ActionOutcome::failed_setup(format!("GET {url} failed: {error}"));
        }
    };

    if !response.status().is_success() {
        return ActionOutcome::failed_product(format!(
            "GET {url} returned {}",
            response.status().as_u16()
        ));
    }

    let status: StatusResponse = match response.json().await {
        Ok(body) => body,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "{url} returned an unreadable body: {error}"
            ));
        }
    };

    if status.status != "ok" {
        return ActionOutcome::failed_product(format!("server reports status `{}`", status.status));
    }

    if status.git_commit == "unknown" {
        return ActionOutcome::failed_setup(
            "server reports an unknown commit; the image was built without its source revision, \
             so the identity assertion would compare nothing"
                .to_string(),
        );
    }

    let expected = &runner.expectation.image_revision;
    if !expected.is_empty()
        && !expected.starts_with(&status.git_commit)
        && !status.git_commit.starts_with(expected.as_str())
    {
        return ActionOutcome::failed_setup(format!(
            "server reports commit `{}` but the artifact under test is `{expected}`",
            status.git_commit
        ));
    }

    if status.version.is_empty() || status.environment.is_empty() {
        return ActionOutcome::failed_product(
            "server reported an empty version or environment".to_string(),
        );
    }

    ActionOutcome::Passed
}
