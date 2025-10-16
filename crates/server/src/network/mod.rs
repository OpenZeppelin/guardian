pub mod miden;

/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetworkType {
    MidenTestnet,
}

impl NetworkType {
    pub fn rpc_endpoint(&self) -> &str {
        match self {
            NetworkType::MidenTestnet => "https://rpc.testnet.miden.io",
        }
    }
}

impl Default for NetworkType {
    fn default() -> Self {
        Self::MidenTestnet
    }
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::MidenTestnet => write!(f, "MidenTestnet"),
        }
    }
}
