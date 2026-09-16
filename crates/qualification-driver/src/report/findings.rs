use serde::{Deserialize, Serialize};

/// A workaround the harness had to apply that a consumer installing the same
/// artifact would also need. Recording it keeps a published-pairing pass from
/// speaking for something a consumer cannot actually do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerFinding {
    pub artifact: String,
    pub summary: String,
    pub workaround: String,
}

impl ConsumerFinding {
    pub fn new(
        artifact: impl Into<String>,
        summary: impl Into<String>,
        workaround: impl Into<String>,
    ) -> Self {
        Self {
            artifact: artifact.into(),
            summary: summary.into(),
            workaround: workaround.into(),
        }
    }
}
