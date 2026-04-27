use crate::aws_cli::{run_aws, run_aws_interactive};
use crate::cleanup_manifest::{
    CleanupAwsTarget, CleanupManifest, CleanupStatus, VerificationSummary,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chrono::Utc;
use std::path::Path;

const CLEANUP_SUMMARY_PREFIX: &str = "BENCH_CLEANUP_SUMMARY=";
const DATABASE_URL_ENV: &str = "DATABASE_URL";
const ECS_EXEC_BATCH_SIZE: usize = 100;

pub async fn run(manifest: &Path) -> Result<()> {
    let updated = purge_manifest_path(manifest).await?;
    print_summary(&updated);
    if updated.purge_status != CleanupStatus::Complete {
        bail!("cleanup finished with status {:?}", updated.purge_status);
    }
    Ok(())
}

pub async fn purge_manifest_path(manifest_path: &Path) -> Result<CleanupManifest> {
    let mut manifest = CleanupManifest::load_from_path(manifest_path)?;
    let result = purge_manifest(&mut manifest).await;
    if let Err(error) = &result {
        manifest.purge_status = CleanupStatus::Failed;
        manifest.purged_at = Some(Utc::now());
        manifest.write_to_path(manifest_path)?;
        return Err(anyhow!(
            "failed to purge benchmark data for run {}: {}",
            manifest.run_id,
            error
        ));
    }
    manifest.write_to_path(manifest_path)?;
    Ok(manifest)
}

async fn purge_manifest(manifest: &mut CleanupManifest) -> Result<()> {
    if manifest.accounts.is_empty() {
        manifest.purge_status = CleanupStatus::Complete;
        manifest.purged_at = Some(Utc::now());
        manifest.verification_summary = Some(VerificationSummary {
            states_remaining: 0,
            deltas_remaining: 0,
            delta_proposals_remaining: 0,
            account_metadata_remaining: 0,
        });
        return Ok(());
    }

    let verification_summary = purge_via_ecs_exec(manifest).await?;
    manifest.purge_status = if is_clean(&verification_summary) {
        CleanupStatus::Complete
    } else {
        CleanupStatus::Partial
    };
    manifest.purged_at = Some(Utc::now());
    manifest.verification_summary = Some(verification_summary);
    Ok(())
}

async fn purge_via_ecs_exec(manifest: &CleanupManifest) -> Result<VerificationSummary> {
    let target = &manifest.cleanup_target.aws;
    let task_arn = resolve_task_arn(target).await?;
    let account_ids = manifest
        .accounts
        .iter()
        .map(|account| account.account_id.clone())
        .collect::<Vec<_>>();
    let mut combined = VerificationSummary {
        states_remaining: 0,
        deltas_remaining: 0,
        delta_proposals_remaining: 0,
        account_metadata_remaining: 0,
    };

    for account_chunk in account_ids.chunks(ECS_EXEC_BATCH_SIZE) {
        let remote_command = build_remote_cleanup_command(&build_cleanup_sql(account_chunk));
        let output = run_aws_interactive(
            &[
                "ecs",
                "execute-command",
                "--cluster",
                target.ecs_cluster.as_str(),
                "--task",
                task_arn.as_str(),
                "--container",
                target.ecs_container.as_str(),
                "--interactive",
                "--command",
                remote_command.as_str(),
                "--region",
                target.region.as_str(),
            ],
            target.profile.as_deref(),
            Some(target.region.as_str()),
        )
        .await
        .with_context(|| {
            format!(
                "failed to execute remote cleanup inside ECS task for batch of {} account ids",
                account_chunk.len()
            )
        })?;

        let summary_line = output
            .lines()
            .find_map(|line| line.strip_prefix(CLEANUP_SUMMARY_PREFIX))
            .ok_or_else(|| anyhow!("cleanup summary was not emitted by remote cleanup command"))?;
        if summary_line.trim().is_empty() {
            bail!("remote cleanup command returned an empty verification summary");
        }

        let summary: VerificationSummary = serde_json::from_str(summary_line)
            .context("failed to parse remote cleanup verification summary")?;
        combined = merge_verification_summaries(combined, summary);
    }

    Ok(combined)
}

async fn resolve_task_arn(target: &CleanupAwsTarget) -> Result<String> {
    let task_arn = run_aws(
        &[
            "ecs",
            "list-tasks",
            "--cluster",
            target.ecs_cluster.as_str(),
            "--service-name",
            target.ecs_service.as_str(),
            "--region",
            target.region.as_str(),
            "--query",
            "taskArns[0]",
            "--output",
            "text",
        ],
        target.profile.as_deref(),
        Some(target.region.as_str()),
    )
    .await
    .context("failed to resolve ECS task ARN for cleanup")?;
    if task_arn.trim().is_empty() || task_arn.trim() == "None" {
        bail!(
            "no running ECS task found for service {}",
            target.ecs_service
        );
    }
    Ok(task_arn)
}

fn build_remote_cleanup_command(sql: &str) -> String {
    let sql_base64 = base64::engine::general_purpose::STANDARD.encode(sql);
    format!(
        "sh -lc 'set -e; tmp=$(mktemp); printf %s {sql_base64} | base64 -d > \"$tmp\"; summary=$(psql \"${{{DATABASE_URL_ENV}}}\" -X -q -t -A -v ON_ERROR_STOP=1 -f \"$tmp\"); rm -f \"$tmp\"; printf \"{CLEANUP_SUMMARY_PREFIX}%s\\n\" \"$summary\"'"
    )
}

fn build_cleanup_sql(account_ids: &[String]) -> String {
    let account_ids = sql_text_array(account_ids);
    let account_array = format!("ARRAY[{account_ids}]::varchar[]");
    format!(
        "BEGIN;
DELETE FROM delta_proposals
WHERE account_id = ANY({account_array});
DELETE FROM deltas
WHERE account_id = ANY({account_array});
DELETE FROM account_metadata
WHERE account_id = ANY({account_array});
DELETE FROM states
WHERE account_id = ANY({account_array});
SELECT json_build_object(
  'states_remaining', (SELECT COUNT(*) FROM states WHERE account_id = ANY({account_array})),
  'deltas_remaining', (SELECT COUNT(*) FROM deltas WHERE account_id = ANY({account_array})),
  'delta_proposals_remaining', (SELECT COUNT(*) FROM delta_proposals WHERE account_id = ANY({account_array})),
  'account_metadata_remaining', (SELECT COUNT(*) FROM account_metadata WHERE account_id = ANY({account_array}))
)::text;
COMMIT;"
    )
}

fn sql_text_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_clean(summary: &VerificationSummary) -> bool {
    summary.states_remaining == 0
        && summary.deltas_remaining == 0
        && summary.delta_proposals_remaining == 0
        && summary.account_metadata_remaining == 0
}

fn merge_verification_summaries(
    left: VerificationSummary,
    right: VerificationSummary,
) -> VerificationSummary {
    VerificationSummary {
        states_remaining: left.states_remaining + right.states_remaining,
        deltas_remaining: left.deltas_remaining + right.deltas_remaining,
        delta_proposals_remaining: left.delta_proposals_remaining + right.delta_proposals_remaining,
        account_metadata_remaining: left.account_metadata_remaining
            + right.account_metadata_remaining,
    }
}

fn print_summary(manifest: &CleanupManifest) {
    println!("run_id={}", manifest.run_id);
    println!("cleanup_status={:?}", manifest.purge_status);
    if let Some(summary) = &manifest.verification_summary {
        println!("states_remaining={}", summary.states_remaining);
        println!("deltas_remaining={}", summary.deltas_remaining);
        println!(
            "delta_proposals_remaining={}",
            summary.delta_proposals_remaining
        );
        println!(
            "account_metadata_remaining={}",
            summary.account_metadata_remaining
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ECS_EXEC_BATCH_SIZE, VerificationSummary, build_cleanup_sql, build_remote_cleanup_command,
        is_clean, merge_verification_summaries, sql_text_array,
    };

    #[test]
    fn clean_summary_should_report_clean() {
        assert!(is_clean(&VerificationSummary {
            states_remaining: 0,
            deltas_remaining: 0,
            delta_proposals_remaining: 0,
            account_metadata_remaining: 0,
        }));
    }

    #[test]
    fn non_zero_summary_should_report_not_clean() {
        assert!(!is_clean(&VerificationSummary {
            states_remaining: 1,
            deltas_remaining: 0,
            delta_proposals_remaining: 0,
            account_metadata_remaining: 0,
        }));
    }

    #[test]
    fn sql_array_should_escape_single_quotes() {
        assert_eq!(sql_text_array(&["a'b".to_string()]), "'a''b'");
    }

    #[test]
    fn cleanup_sql_should_reference_all_tables() {
        let sql = build_cleanup_sql(&["0x1".to_string(), "0x2".to_string()]);
        for table in ["delta_proposals", "deltas", "account_metadata", "states"] {
            assert!(sql.contains(table));
        }
        assert!(sql.contains("BEGIN;"));
        assert!(sql.contains("COMMIT;"));
        assert!(sql.contains("json_build_object"));
        assert!(sql.contains("'0x1'"));
        assert!(sql.contains("'0x2'"));
    }

    #[test]
    fn remote_cleanup_command_should_use_database_url_env() {
        let command = build_remote_cleanup_command("SELECT 1;");
        assert!(command.contains("${DATABASE_URL}"));
    }

    #[test]
    fn verification_summaries_should_merge_by_summing_counts() {
        let merged = merge_verification_summaries(
            VerificationSummary {
                states_remaining: 1,
                deltas_remaining: 2,
                delta_proposals_remaining: 3,
                account_metadata_remaining: 4,
            },
            VerificationSummary {
                states_remaining: 5,
                deltas_remaining: 6,
                delta_proposals_remaining: 7,
                account_metadata_remaining: 8,
            },
        );

        assert_eq!(merged.states_remaining, 6);
        assert_eq!(merged.deltas_remaining, 8);
        assert_eq!(merged.delta_proposals_remaining, 10);
        assert_eq!(merged.account_metadata_remaining, 12);
    }

    #[test]
    fn ecs_exec_batch_size_should_be_bounded_for_large_runs() {
        const {
            assert!(ECS_EXEC_BATCH_SIZE <= 100);
        }
    }
}
