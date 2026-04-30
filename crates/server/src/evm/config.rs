use std::collections::BTreeMap;

use crate::metadata::network::normalize_evm_address;

const RPC_URLS_ENV: &str = "GUARDIAN_EVM_RPC_URLS";
const ENTRYPOINT_ADDRESS_ENV: &str = "GUARDIAN_EVM_ENTRYPOINT_ADDRESS";
const DEFAULT_ENTRYPOINT_ADDRESS: &str = "0x433709009b8330fda32311df1c2afa402ed8d009";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmChainConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    pub entrypoint_address: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvmChainRegistry {
    chains: BTreeMap<u64, EvmChainConfig>,
}

impl EvmChainRegistry {
    pub fn from_env() -> Result<Self, String> {
        let rpc_urls = parse_map(RPC_URLS_ENV)?;
        let entrypoint_address = std::env::var(ENTRYPOINT_ADDRESS_ENV)
            .unwrap_or_else(|_| DEFAULT_ENTRYPOINT_ADDRESS.to_string());

        Self::from_rpc_urls(rpc_urls, &entrypoint_address)
    }

    fn from_rpc_urls(
        rpc_urls: BTreeMap<u64, String>,
        entrypoint_address: &str,
    ) -> Result<Self, String> {
        let entrypoint_address = normalize_evm_address(entrypoint_address)
            .map_err(|error| format!("{ENTRYPOINT_ADDRESS_ENV}: {error}"))?;
        let mut chains = BTreeMap::new();

        for (chain_id, rpc_url) in rpc_urls {
            if !is_http_url(&rpc_url) {
                return Err(format!(
                    "{RPC_URLS_ENV} entry for chain {chain_id} must be an http or https URL"
                ));
            }

            chains.insert(
                chain_id,
                EvmChainConfig {
                    chain_id,
                    rpc_url,
                    entrypoint_address: entrypoint_address.clone(),
                },
            );
        }

        Ok(Self { chains })
    }

    pub fn new(chains: impl IntoIterator<Item = EvmChainConfig>) -> Self {
        Self {
            chains: chains
                .into_iter()
                .map(|config| (config.chain_id, config))
                .collect(),
        }
    }

    pub fn get(&self, chain_id: u64) -> Option<&EvmChainConfig> {
        self.chains.get(&chain_id)
    }

    pub fn supported_chains(&self) -> Vec<u64> {
        self.chains.keys().copied().collect()
    }
}

fn parse_map(env_var: &str) -> Result<BTreeMap<u64, String>, String> {
    let Ok(raw) = std::env::var(env_var) else {
        return Ok(BTreeMap::new());
    };

    let mut values = BTreeMap::new();
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (chain_id, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("{env_var} entries must use chain_id=value format"))?;
        let chain_id = chain_id
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{env_var} chain ID '{chain_id}' is invalid"))?;
        if chain_id == 0 {
            return Err(format!("{env_var} chain ID must be greater than zero"));
        }
        values.insert(chain_id, value.trim().to_string());
    }

    Ok(values)
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_indexes_configured_rpc_chains() {
        let registry = EvmChainRegistry::new(vec![EvmChainConfig {
            chain_id: 31337,
            rpc_url: "http://127.0.0.1:8545".to_string(),
            entrypoint_address: DEFAULT_ENTRYPOINT_ADDRESS.to_string(),
        }]);

        assert_eq!(registry.supported_chains(), vec![31337]);
        assert!(registry.get(31337).is_some());
    }

    #[test]
    fn registry_uses_one_entrypoint_address_for_all_rpc_chains() {
        let registry = EvmChainRegistry::from_rpc_urls(
            BTreeMap::from([
                (1, "https://ethereum-rpc.publicnode.com".to_string()),
                (
                    11155111,
                    "https://ethereum-sepolia-rpc.publicnode.com".to_string(),
                ),
            ]),
            DEFAULT_ENTRYPOINT_ADDRESS,
        )
        .expect("valid EVM chain registry");

        assert_eq!(registry.supported_chains(), vec![1, 11155111]);
        assert_eq!(
            registry.get(1).expect("mainnet").entrypoint_address,
            DEFAULT_ENTRYPOINT_ADDRESS
        );
        assert_eq!(
            registry.get(11155111).expect("sepolia").entrypoint_address,
            DEFAULT_ENTRYPOINT_ADDRESS
        );
    }

    #[test]
    fn registry_rejects_invalid_entrypoint_address() {
        let error = EvmChainRegistry::from_rpc_urls(
            BTreeMap::from([(31337, "http://127.0.0.1:8545".to_string())]),
            "0x1234",
        )
        .expect_err("invalid EntryPoint address should fail");

        assert!(error.contains(ENTRYPOINT_ADDRESS_ENV));
    }

    #[test]
    fn registry_rejects_non_http_rpc_url() {
        let error = EvmChainRegistry::from_rpc_urls(
            BTreeMap::from([(31337, "ws://127.0.0.1:8545".to_string())]),
            DEFAULT_ENTRYPOINT_ADDRESS,
        )
        .expect_err("non-http RPC URL should fail");

        assert!(error.contains(RPC_URLS_ENV));
    }
}
