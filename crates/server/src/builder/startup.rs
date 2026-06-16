//! One-shot startup summary of the resolved, non-secret server
//! configuration.
//!
//! Assembled by [`ServerBuilder::build`] from the values it actually
//! wired and emitted once as the server starts (at the top of
//! [`ServerHandle::run`], before any listener binds), so operators can
//! confirm which version, backends, network, and signers a process is
//! running without reconstructing it from environment variables. Only
//! backend *kinds* and ports are surfaced — never connection strings,
//! key identifiers, or credentials.

use crate::build_info;
use crate::canonicalization::CanonicalizationConfig;
use crate::network::NetworkType;
use crate::storage::StorageType;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupInfo {
    network: NetworkType,
    storage: StorageType,
    ecdsa_backend: &'static str,
    falcon_commitment: String,
    ecdsa_commitment: String,
    canonicalization: Option<CanonicalizationConfig>,
    operator_count: usize,
    cursor_secret_configured: bool,
    http_port: Option<u16>,
    grpc_port: Option<u16>,
    metrics_addr: Option<SocketAddr>,
}

impl StartupInfo {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        network: NetworkType,
        storage: StorageType,
        ecdsa_backend: &'static str,
        falcon_commitment: String,
        ecdsa_commitment: String,
        canonicalization: Option<CanonicalizationConfig>,
        operator_count: usize,
        cursor_secret_configured: bool,
        http_port: Option<u16>,
        grpc_port: Option<u16>,
        metrics_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            network,
            storage,
            ecdsa_backend,
            falcon_commitment,
            ecdsa_commitment,
            canonicalization,
            operator_count,
            cursor_secret_configured,
            http_port,
            grpc_port,
            metrics_addr,
        }
    }

    pub(crate) fn log(&self) {
        tracing::info!("===== Guardian server configuration =====");
        tracing::info!(
            version = build_info::VERSION,
            git_sha = build_info::GIT_SHA,
            profile = build_info::build_profile(),
            "Guardian server starting"
        );
        tracing::info!(
            network = %self.network,
            rpc_endpoint = self.network.rpc_endpoint(),
            "network"
        );
        tracing::info!(storage = %self.storage, "storage backend");
        tracing::info!(
            falcon = "enabled",
            falcon_commitment = %self.falcon_commitment,
            ecdsa_backend = self.ecdsa_backend,
            ecdsa_commitment = %self.ecdsa_commitment,
            "ack signers"
        );
        tracing::info!(
            operators = self.operator_count,
            cursor_secret = if self.cursor_secret_configured {
                "configured"
            } else {
                "ephemeral"
            },
            "dashboard"
        );
        match &self.canonicalization {
            Some(config) => tracing::info!(
                check_interval_seconds = config.check_interval_seconds,
                max_retries = config.max_retries,
                submission_grace_period_seconds = config.submission_grace_period_seconds,
                "canonicalization"
            ),
            None => {
                tracing::info!("optimistic mode (deltas accepted without on-chain verification)")
            }
        }
        tracing::info!(
            http = %port_label(self.http_port),
            grpc = %port_label(self.grpc_port),
            metrics = %self
                .metrics_addr
                .map(|addr| addr.to_string())
                .unwrap_or_else(|| "disabled".to_string()),
            "listeners"
        );
        tracing::info!(features = ?compiled_features(), "compiled features");
        tracing::info!("=========================================");
    }
}

fn port_label(port: Option<u16>) -> String {
    match port {
        Some(port) => port.to_string(),
        None => "disabled".to_string(),
    }
}

fn compiled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "postgres") {
        features.push("postgres");
    }
    if cfg!(feature = "evm") {
        features.push("evm");
    }
    features
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn captures_postgres_kms_and_canonicalization_config() {
        let info = StartupInfo::new(
            NetworkType::MidenDevnet,
            StorageType::Postgres,
            "aws-kms",
            "0xfalcon".to_string(),
            "0xecdsa".to_string(),
            Some(CanonicalizationConfig {
                check_interval_seconds: 10,
                max_retries: 48,
                submission_grace_period_seconds: 600,
            }),
            3,
            true,
            Some(3000),
            Some(50051),
            Some("127.0.0.1:9464".parse().unwrap()),
        );

        assert_eq!(info.network, NetworkType::MidenDevnet);
        assert_eq!(info.storage, StorageType::Postgres);
        assert_eq!(info.ecdsa_backend, "aws-kms");
        assert_eq!(info.falcon_commitment, "0xfalcon");
        assert_eq!(info.ecdsa_commitment, "0xecdsa");
        assert_eq!(info.operator_count, 3);
        assert!(info.cursor_secret_configured);
        assert_eq!(info.http_port, Some(3000));
        assert_eq!(info.grpc_port, Some(50051));
        assert_eq!(info.metrics_addr, Some("127.0.0.1:9464".parse().unwrap()));
        assert_eq!(
            info.canonicalization.as_ref().map(|c| c.max_retries),
            Some(48)
        );
    }

    #[test]
    fn optimistic_mode_and_disabled_listeners_are_none() {
        let info = StartupInfo::new(
            NetworkType::MidenLocal,
            StorageType::Filesystem,
            "in-memory",
            "0xfalcon".to_string(),
            "0xecdsa".to_string(),
            None,
            0,
            false,
            None,
            None,
            None,
        );

        assert_eq!(info.storage, StorageType::Filesystem);
        assert_eq!(info.ecdsa_backend, "in-memory");
        assert_eq!(info.canonicalization, None);
        assert_eq!(info.operator_count, 0);
        assert!(!info.cursor_secret_configured);
        assert_eq!(info.http_port, None);
        assert_eq!(info.grpc_port, None);
    }

    #[test]
    fn port_label_renders_number_or_disabled() {
        assert_eq!(port_label(Some(3000)), "3000");
        assert_eq!(port_label(None), "disabled");
    }

    #[test]
    fn compiled_features_reflect_enabled_cargo_features() {
        let features = compiled_features();
        assert_eq!(features.contains(&"postgres"), cfg!(feature = "postgres"));
        assert_eq!(features.contains(&"evm"), cfg!(feature = "evm"));
    }
}
