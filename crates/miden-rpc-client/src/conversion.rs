use miden_protocol::account::AccountId;
use miden_protocol::block::{BlockHeader, FeeParameters, ValidatorKeys};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey;
use miden_protocol::crypto::merkle::MerklePath;
use miden_protocol::crypto::merkle::mmr::{Forest, MmrDelta};
use miden_protocol::utils::serde::Deserializable;
use miden_protocol::{Felt, Word};

use crate::{blockchain, primitives};

impl TryFrom<primitives::Digest> for Word {
    type Error = String;

    fn try_from(value: primitives::Digest) -> Result<Self, Self::Error> {
        let felt = |value| Felt::new(value).map_err(|e| format!("invalid RPC digest: {e}"));
        Ok(Self::new([
            felt(value.d0)?,
            felt(value.d1)?,
            felt(value.d2)?,
            felt(value.d3)?,
        ]))
    }
}

impl TryFrom<primitives::MerklePath> for MerklePath {
    type Error = String;

    fn try_from(value: primitives::MerklePath) -> Result<Self, Self::Error> {
        Ok(Self::new(
            value
                .siblings
                .into_iter()
                .map(Word::try_from)
                .collect::<Result<_, _>>()?,
        ))
    }
}

impl TryFrom<primitives::MmrDelta> for MmrDelta {
    type Error = String;

    fn try_from(value: primitives::MmrDelta) -> Result<Self, Self::Error> {
        let leaves = usize::try_from(value.forest).map_err(|e| e.to_string())?;
        Ok(Self {
            forest: Forest::new(leaves).map_err(|e| e.to_string())?,
            data: value
                .data
                .into_iter()
                .map(Word::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<blockchain::BlockHeader> for BlockHeader {
    type Error = String;

    fn try_from(value: blockchain::BlockHeader) -> Result<Self, Self::Error> {
        let required = |digest: Option<primitives::Digest>, field: &str| {
            Word::try_from(digest.ok_or_else(|| format!("missing block header {field}"))?)
        };
        let keys = value
            .validator_keys
            .into_iter()
            .map(|key| PublicKey::read_from_bytes(&key.validator_key).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let keys = ValidatorKeys::new(keys).map_err(|e| e.to_string())?;
        let fees = value
            .fee_parameters
            .ok_or("missing block header fee_parameters")?;
        let native_asset = fees.native_asset_id.ok_or("missing native fee asset ID")?;
        let native_asset =
            AccountId::read_from_bytes(&native_asset.id).map_err(|e| e.to_string())?;
        Ok(Self::new(
            value.version,
            required(value.prev_block_commitment, "prev_block_commitment")?,
            value.block_num.into(),
            required(value.chain_commitment, "chain_commitment")?,
            required(value.account_root, "account_root")?,
            required(value.nullifier_root, "nullifier_root")?,
            required(value.note_root, "note_root")?,
            required(value.tx_commitment, "tx_commitment")?,
            required(value.tx_kernel_commitment, "tx_kernel_commitment")?,
            keys,
            FeeParameters::new(native_asset, fees.verification_base_fee),
            value.timestamp,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_noncanonical_digest_in_witness_paths() {
        let digest = primitives::Digest {
            d0: u64::MAX,
            d1: 0,
            d2: 0,
            d3: 0,
        };
        assert!(Word::try_from(digest).is_err());
        assert!(
            MerklePath::try_from(primitives::MerklePath {
                siblings: vec![digest]
            })
            .is_err()
        );
        assert!(
            MmrDelta::try_from(primitives::MmrDelta {
                forest: 1,
                data: vec![digest]
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_missing_block_header_fields() {
        assert!(BlockHeader::try_from(blockchain::BlockHeader::default()).is_err());
    }

    #[test]
    fn preserves_mmr_forest_and_path_siblings() {
        let digest = primitives::Digest {
            d0: 1,
            d1: 2,
            d2: 3,
            d3: 4,
        };
        let word = Word::try_from(digest).unwrap();
        let path = MerklePath::try_from(primitives::MerklePath {
            siblings: vec![digest],
        })
        .unwrap();
        assert_eq!(path, MerklePath::new(vec![word]));
        let delta = MmrDelta::try_from(primitives::MmrDelta {
            forest: 7,
            data: vec![digest],
        })
        .unwrap();
        assert_eq!(delta.forest, Forest::new(7).unwrap());
        assert_eq!(delta.data, vec![word]);
    }
}
