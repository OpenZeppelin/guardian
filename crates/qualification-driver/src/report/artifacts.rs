use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::manifest::ImageSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    Branch,
    Release,
    Published,
}

impl Pairing {
    pub fn image_source(self) -> ImageSource {
        match self {
            Self::Branch => ImageSource::Built,
            Self::Release | Self::Published => ImageSource::Pulled,
        }
    }

    pub fn installs_from_registry(self) -> bool {
        match self {
            Self::Branch | Self::Release => false,
            Self::Published => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSet {
    pub image_digest: String,
    pub image_revision: String,
    pub pairing: Pairing,
    #[serde(default)]
    pub sdk_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub sdk_integrity: BTreeMap<String, String>,
    #[serde(default)]
    pub miden_versions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkewError {
    #[error(
        "image reports revision `{image_revision}` but the installed packages resolve \
         Miden dependency `{dependency}` at `{package_version}` against the image's \
         `{image_version}`; the image and the packages are released by separate pipelines \
         and have diverged"
    )]
    MidenDependencyMismatch {
        image_revision: String,
        dependency: String,
        image_version: String,
        package_version: String,
    },
    #[error("published pairing recorded no package versions, so nothing was actually installed")]
    NoPackagesInstalled,
    #[error("published pairing recorded no integrity hash for `{package}`")]
    MissingIntegrity { package: String },
    #[error("image digest `{0}` is not a resolved digest")]
    UnresolvedDigest(String),
}

impl ArtifactSet {
    pub fn check_digest_resolved(&self) -> Result<(), SkewError> {
        if self.image_digest.starts_with("sha256:") && self.image_digest.len() == 71 {
            Ok(())
        } else {
            Err(SkewError::UnresolvedDigest(self.image_digest.clone()))
        }
    }

    /// Only the published pairing can drift: the other two take their packages
    /// from the same ref the image was built from.
    pub fn check_skew(
        &self,
        image_miden_versions: &BTreeMap<String, String>,
    ) -> Result<(), Vec<SkewError>> {
        if !self.pairing.installs_from_registry() {
            return Ok(());
        }
        let mut errors = Vec::new();
        if self.sdk_versions.is_empty() {
            errors.push(SkewError::NoPackagesInstalled);
        }
        for package in self.sdk_versions.keys() {
            if !self.sdk_integrity.contains_key(package) {
                errors.push(SkewError::MissingIntegrity {
                    package: package.clone(),
                });
            }
        }
        for (dependency, image_version) in image_miden_versions {
            if let Some(package_version) = self.miden_versions.get(dependency)
                && package_version != image_version
            {
                errors.push(SkewError::MidenDependencyMismatch {
                    image_revision: self.image_revision.clone(),
                    dependency: dependency.clone(),
                    image_version: image_version.clone(),
                    package_version: package_version.clone(),
                });
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(miden: BTreeMap<String, String>) -> ArtifactSet {
        ArtifactSet {
            image_digest: format!("sha256:{}", "a".repeat(64)),
            image_revision: "383bc1d4".to_string(),
            pairing: Pairing::Published,
            sdk_versions: BTreeMap::from([("miden-multisig-client".into(), "0.17.0".into())]),
            sdk_integrity: BTreeMap::from([("miden-multisig-client".into(), "sha512-x".into())]),
            miden_versions: miden,
        }
    }

    #[test]
    fn resolved_digest_is_accepted() {
        assert!(published(BTreeMap::new()).check_digest_resolved().is_ok());
    }

    #[test]
    fn floating_tag_is_rejected() {
        let mut set = published(BTreeMap::new());
        set.image_digest = "latest".into();
        assert!(set.check_digest_resolved().is_err());
    }

    #[test]
    fn matching_dependency_versions_pass() {
        let versions = BTreeMap::from([("miden-protocol".to_string(), "0.16.1".to_string())]);
        assert!(published(versions.clone()).check_skew(&versions).is_ok());
    }

    #[test]
    fn diverged_dependency_versions_are_reported() {
        let package = BTreeMap::from([("miden-protocol".to_string(), "0.16.0".to_string())]);
        let image = BTreeMap::from([("miden-protocol".to_string(), "0.16.1".to_string())]);
        let errors = published(package).check_skew(&image).unwrap_err();
        assert!(matches!(
            errors.as_slice(),
            [SkewError::MidenDependencyMismatch { .. }]
        ));
    }

    #[test]
    fn branch_pairing_is_exempt_from_skew() {
        let mut set = published(BTreeMap::from([("m".into(), "1".into())]));
        set.pairing = Pairing::Branch;
        let image = BTreeMap::from([("m".to_string(), "2".to_string())]);
        assert!(set.check_skew(&image).is_ok());
    }

    #[test]
    fn published_pairing_without_packages_is_rejected() {
        let mut set = published(BTreeMap::new());
        set.sdk_versions.clear();
        set.sdk_integrity.clear();
        let errors = set.check_skew(&BTreeMap::new()).unwrap_err();
        assert!(errors.contains(&SkewError::NoPackagesInstalled));
    }

    #[test]
    fn published_pairing_without_integrity_is_rejected() {
        let mut set = published(BTreeMap::new());
        set.sdk_integrity.clear();
        let errors = set.check_skew(&BTreeMap::new()).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SkewError::MissingIntegrity { package } if package == "miden-multisig-client"
        )));
    }
}
