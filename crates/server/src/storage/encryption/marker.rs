use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::envelope::ENVELOPE_VERSION;
use super::key_provider::StorageKeyProvider;

/// Written only when a store is encrypted; its presence means the store is
/// encrypted. `init_kid` is informational lineage and does not pin the active
/// key, so rotation is unaffected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EncryptionMarker {
    pub(crate) scheme_version: u8,
    pub(crate) init_kid: String,
}

impl EncryptionMarker {
    pub(crate) fn new(init_kid: String) -> Self {
        Self {
            scheme_version: ENVELOPE_VERSION,
            init_kid,
        }
    }
}

/// Backend-specific persistence of the store-level encryption marker and the
/// emptiness probe the startup guard needs.
#[async_trait]
pub(crate) trait MarkerStore: Send + Sync {
    async fn read_encryption_marker(&self) -> Result<Option<EncryptionMarker>, String>;
    async fn write_encryption_marker(&self, marker: &EncryptionMarker) -> Result<(), String>;
    /// `true` when the store holds any state/delta/proposal payload record. The
    /// marker itself is never counted.
    async fn has_payload_records(&self) -> Result<bool, String>;
}

/// Enforce the consistent-state guard before any write. `provider` is `Some` in
/// encrypted mode and `None` in plaintext mode.
pub(crate) async fn apply_startup_guard(
    store: &dyn MarkerStore,
    provider: Option<&dyn StorageKeyProvider>,
) -> Result<(), String> {
    let marker = store.read_encryption_marker().await?;
    match (provider, marker) {
        (Some(provider), Some(marker)) => {
            if marker.scheme_version != ENVELOPE_VERSION {
                return Err(format!(
                    "storage encryption marker scheme version {} is not supported by this build (supports {ENVELOPE_VERSION})",
                    marker.scheme_version
                ));
            }
            provider.key(&marker.init_kid).map(|_| ()).map_err(|_| {
                format!(
                    "storage encryption key '{}' recorded in the store marker is not available",
                    marker.init_kid
                )
            })
        }
        (Some(provider), None) => {
            if store.has_payload_records().await? {
                return Err(
                    "refusing to enable storage encryption on a store that already holds \
                     plaintext records; an explicit re-encryption migration is required"
                        .to_string(),
                );
            }
            store
                .write_encryption_marker(&EncryptionMarker::new(
                    provider.active_key_id().to_string(),
                ))
                .await
        }
        (None, Some(_)) => Err(
            "storage is marked encrypted but no storage encryption key is configured".to_string(),
        ),
        (None, None) => Ok(()),
    }
}
