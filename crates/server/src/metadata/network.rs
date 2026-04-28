use crate::api::grpc::guardian::{self, network_config};
use crate::network::NetworkType;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkConfig {
    Miden {
        network_type: MidenNetworkType,
    },
    Evm {
        chain_id: u64,
        account_address: String,
        multisig_module_address: String,
        rpc_endpoint: String,
    },
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MidenNetworkType {
    Local,
    Devnet,
    Testnet,
}

impl NetworkConfig {
    pub fn miden_default() -> Self {
        Self::Miden {
            network_type: MidenNetworkType::from(NetworkType::MidenLocal),
        }
    }

    pub fn is_evm(&self) -> bool {
        matches!(self, Self::Evm { .. })
    }

    pub fn is_miden(&self) -> bool {
        matches!(self, Self::Miden { .. })
    }

    pub fn validate_for_account(&self, account_id: &str) -> Result<Self, String> {
        match self {
            Self::Miden { network_type } => Ok(Self::Miden {
                network_type: *network_type,
            }),
            Self::Evm {
                chain_id,
                account_address,
                multisig_module_address,
                rpc_endpoint,
            } => {
                if *chain_id == 0 {
                    return Err("chain_id must be greater than zero".to_string());
                }

                let account_address = normalize_evm_address(account_address)?;
                let multisig_module_address = normalize_evm_address(multisig_module_address)?;
                if !is_http_url(rpc_endpoint) {
                    return Err("rpc_endpoint must be an http or https URL".to_string());
                }

                let expected = evm_account_id(*chain_id, &account_address);
                if account_id != expected {
                    return Err(format!(
                        "account_id must be '{}', got '{}'",
                        expected, account_id
                    ));
                }

                Ok(Self::Evm {
                    chain_id: *chain_id,
                    account_address,
                    multisig_module_address,
                    rpc_endpoint: rpc_endpoint.clone(),
                })
            }
        }
    }
}

impl From<NetworkType> for MidenNetworkType {
    fn from(value: NetworkType) -> Self {
        match value {
            NetworkType::MidenLocal => Self::Local,
            NetworkType::MidenDevnet => Self::Devnet,
            NetworkType::MidenTestnet => Self::Testnet,
        }
    }
}

impl From<MidenNetworkType> for NetworkType {
    fn from(value: MidenNetworkType) -> Self {
        match value {
            MidenNetworkType::Local => Self::MidenLocal,
            MidenNetworkType::Devnet => Self::MidenDevnet,
            MidenNetworkType::Testnet => Self::MidenTestnet,
        }
    }
}

impl std::fmt::Display for MidenNetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Devnet => f.write_str("devnet"),
            Self::Testnet => f.write_str("testnet"),
        }
    }
}

impl TryFrom<guardian::NetworkConfig> for NetworkConfig {
    type Error = String;

    fn try_from(config: guardian::NetworkConfig) -> Result<Self, Self::Error> {
        match config.network_type {
            Some(network_config::NetworkType::Miden(miden)) => {
                let network_type = match miden.network_type.to_ascii_lowercase().as_str() {
                    "local" | "midenlocal" => MidenNetworkType::Local,
                    "devnet" | "midendevnet" => MidenNetworkType::Devnet,
                    "testnet" | "midentestnet" => MidenNetworkType::Testnet,
                    other => return Err(format!("unsupported Miden network_type: {other}")),
                };
                Ok(Self::Miden { network_type })
            }
            Some(network_config::NetworkType::Evm(evm)) => Ok(Self::Evm {
                chain_id: evm.chain_id,
                account_address: evm.account_address,
                multisig_module_address: evm.multisig_module_address,
                rpc_endpoint: evm.rpc_endpoint,
            }),
            None => Err("Network type not specified".to_string()),
        }
    }
}

pub fn evm_account_id(chain_id: u64, account_address: &str) -> String {
    format!("evm:{chain_id}:{account_address}")
}

pub fn is_evm_account_id(account_id: &str) -> bool {
    account_id.starts_with("evm:")
}

pub fn normalize_evm_address(address: &str) -> Result<String, String> {
    let address = address.trim();
    let clean = address
        .strip_prefix("0x")
        .ok_or_else(|| "address must start with 0x".to_string())?;
    if clean.len() != 40 {
        return Err("address must be 20 bytes".to_string());
    }
    if !clean.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        return Err("address must be hex encoded".to_string());
    }
    Ok(format!("0x{}", clean.to_ascii_lowercase()))
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_evm_address_to_lowercase() {
        let address = normalize_evm_address("0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD")
            .expect("valid address");

        assert_eq!(address, "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd");
    }

    #[test]
    fn validates_evm_account_id() {
        let config = NetworkConfig::Evm {
            chain_id: 1,
            account_address: "0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD".to_string(),
            multisig_module_address: "0x1111111111111111111111111111111111111111".to_string(),
            rpc_endpoint: "http://localhost:8545".to_string(),
        };

        let normalized = config
            .validate_for_account("evm:1:0xabcdefabcdefabcdefabcdefabcdefabcdefabcd")
            .expect("valid network config");

        match normalized {
            NetworkConfig::Evm {
                account_address, ..
            } => assert_eq!(
                account_address,
                "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"
            ),
            NetworkConfig::Miden { .. } => panic!("expected evm config"),
        }
    }
}
