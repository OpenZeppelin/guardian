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

/// What a run ran against, carried into the result so a pass names the thing
/// it passed against.
///
/// `sdk_versions`, `sdk_integrity` and `miden_versions` are empty on every
/// pairing that exists today, because both of those install the SDKs from this
/// workspace rather than from a registry. They are here because the result
/// schema carries them; the checks that would compare them belong with the
/// `published` pairing, and are worth writing when something can actually
/// install from the registry rather than now, against nothing.
impl ArtifactSet {
    pub fn installs_from_registry(&self) -> bool {
        self.pairing.installs_from_registry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_published_pairing_claims_a_registry_install() {
        assert!(!Pairing::Branch.installs_from_registry());
        assert!(!Pairing::Release.installs_from_registry());
        assert!(Pairing::Published.installs_from_registry());
    }

    #[test]
    fn a_pairing_names_the_image_source_it_requires() {
        assert_eq!(Pairing::Branch.image_source(), ImageSource::Built);
        assert_eq!(Pairing::Release.image_source(), ImageSource::Pulled);
        assert_eq!(Pairing::Published.image_source(), ImageSource::Pulled);
    }
}
