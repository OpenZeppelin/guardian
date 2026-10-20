//! Public Miden networks the SDK presets on top of miden-client's own
//! mainnet, testnet and devnet presets.

use miden_client::rpc::Endpoint;
use miden_protocol::address::NetworkId;

/// The Miden Cedar network. miden-client ships no preset for it,
/// so [`crate::MultisigClientBuilder`] derives the remote prover and note
/// transport from these endpoints when given [`MidenCedar::endpoint`].
///
/// Cedar addresses use the prefix [`MidenCedar::ADDRESS_PREFIX`] (`mm`, today
/// the same string as the protocol's mainnet prefix): encode them with
/// [`MidenCedar::network_id`]. miden-client does not know Cedar, so for a Cedar
/// endpoint [`Endpoint::to_network_id`] and miden-client's public
/// `Client::network_id()` return its generic custom id `mcst` instead. Do not
/// encode Cedar addresses with either; they would not match what Cedar
/// wallets and explorers show.
pub struct MidenCedar;

impl MidenCedar {
    pub const RPC_HOST: &'static str = "rpc.cedar.miden.io";
    pub const PROVER_ENDPOINT: &'static str = "https://tx-prover.cedar.miden.io";
    pub const NOTE_TRANSPORT_ENDPOINT: &'static str = "https://transport.cedar.miden.io";
    /// Bech32 human-readable prefix of Cedar addresses. Kept separate from the
    /// protocol's mainnet prefix so Cedar stays on `mm` if mainnet moves.
    pub const ADDRESS_PREFIX: &'static str = "mm";

    /// The Cedar node RPC endpoint, `https://rpc.cedar.miden.io`.
    pub fn endpoint() -> Endpoint {
        Endpoint::new("https".to_string(), Self::RPC_HOST.to_string(), None)
    }

    /// The network ID Cedar addresses are encoded with.
    pub fn network_id() -> NetworkId {
        NetworkId::new(Self::ADDRESS_PREFIX).expect("Cedar address prefix is a valid network ID")
    }

    pub(crate) fn matches(endpoint: &Endpoint) -> bool {
        endpoint == &Self::endpoint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_cedar_url_matches_the_preset() {
        let parsed = Endpoint::try_from("https://rpc.cedar.miden.io").unwrap();
        assert!(MidenCedar::matches(&parsed));
        assert_eq!(
            MidenCedar::network_id().as_str(),
            MidenCedar::ADDRESS_PREFIX
        );
    }
}
