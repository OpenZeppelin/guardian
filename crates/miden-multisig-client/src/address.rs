//! Account ids as people hand them around: bech32 addresses, which is what the
//! faucet pages and explorers show, or `0x`-prefixed hex.

use miden_protocol::account::AccountId;
use miden_protocol::address::NetworkId;

use crate::error::{MultisigError, Result};

/// Parses an account id given as a bech32 address (`mdev1...`, `mtst1...`) or
/// as `0x`-prefixed hex. Use it for values copied from a faucet page, an
/// explorer or an environment variable, such as the chain's fee faucet.
pub fn parse_account_id(input: &str) -> Result<AccountId> {
    parse_account_address(input).map(|(_, account_id)| account_id)
}

/// [`parse_account_id`] that also returns the network a bech32 address names,
/// for callers that check it against the endpoint they use. Hex carries no
/// network and yields `None`.
pub fn parse_account_address(input: &str) -> Result<(Option<NetworkId>, AccountId)> {
    let input = input.trim();
    if let Some(digits) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        AccountId::from_hex(&format!("0x{digits}"))
            .map(|account_id| (None, account_id))
            .map_err(|error| {
                MultisigError::InvalidConfig(format!("invalid account id '{input}': {error}"))
            })
    } else {
        AccountId::from_bech32(input)
            .map(|(network_id, account_id)| (Some(network_id), account_id))
            .map_err(|error| {
                MultisigError::InvalidConfig(format!(
                    "invalid account address '{input}': {error}; expected a bech32 address or 0x-prefixed hex"
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT_HEX: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

    #[test]
    fn parses_hex_with_either_prefix_case() {
        let expected = AccountId::from_hex(ACCOUNT_HEX).unwrap();
        assert_eq!(parse_account_id(ACCOUNT_HEX).unwrap(), expected);
        assert_eq!(
            parse_account_id(&ACCOUNT_HEX.replace("0x", "0X")).unwrap(),
            expected
        );
        assert_eq!(
            parse_account_address(ACCOUNT_HEX).unwrap(),
            (None, expected)
        );
    }

    #[test]
    fn parses_bech32_and_reports_its_network() {
        let account_id = AccountId::from_hex(ACCOUNT_HEX).unwrap();
        for network in [NetworkId::Devnet, NetworkId::Testnet] {
            let address = account_id.to_bech32(network.clone());
            assert_eq!(parse_account_id(&address).unwrap(), account_id);
            assert_eq!(
                parse_account_address(&format!("  {address}  ")).unwrap(),
                (Some(network), account_id)
            );
        }
    }

    #[test]
    fn rejects_anything_else_by_name() {
        let error = parse_account_id("faucet").unwrap_err().to_string();
        assert!(
            error.contains("invalid account address 'faucet'"),
            "{error}"
        );
        let error = parse_account_id("0xnope").unwrap_err().to_string();
        assert!(error.contains("invalid account id '0xnope'"), "{error}");
    }
}
