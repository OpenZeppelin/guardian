use miden_rpc_client::MidenRpcClient;

fn digest_hex(d0: u64, d1: u64, d2: u64, d3: u64) -> String {
    let mut bytes = Vec::with_capacity(32);
    for v in [d0, d1, d2, d3] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    format!("0x{}", hex::encode(bytes))
}

#[tokio::main]
async fn main() {
    for endpoint in [
        "https://rpc.devnet.miden.io",
        "https://rpc.testnet.miden.io",
    ] {
        match MidenRpcClient::connect(endpoint).await {
            Ok(mut client) => match client.get_block_header(None, false).await {
                Ok(resp) => match resp.block_header.and_then(|h| h.tx_kernel_commitment) {
                    Some(k) => println!(
                        "{endpoint}: block kernel commitment = {}",
                        digest_hex(k.d0, k.d1, k.d2, k.d3)
                    ),
                    None => println!("{endpoint}: header missing kernel commitment"),
                },
                Err(e) => println!("{endpoint}: header error: {e}"),
            },
            Err(e) => println!("{endpoint}: connect error: {e}"),
        }
    }
    println!("local client kernel (beta.1)   = 0x9b3876970730deff3fc4e1d90d68b0578ce19c6e5bd58a0ac5774dc65dbea1d7");
    println!("failing advice-map key (alpha) = 0x60e15da40818dc87d8a04daee51e98ff4d6af6b2a24819a56abacefc09adb730");
}
