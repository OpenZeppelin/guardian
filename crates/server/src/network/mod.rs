/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetworkType {
    Miden,
}

impl Default for NetworkType {
    fn default() -> Self {
        Self::Miden
    }
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::Miden => write!(f, "Miden"),
        }
    }
}
