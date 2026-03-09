use miden_multisig_client::{Asset, MultisigAccount};

pub fn shorten_hex(hex: &str) -> String {
    if hex.len() <= 12 {
        return hex.to_string();
    }

    let prefix = &hex[..6];
    let suffix = &hex[hex.len() - 4..];
    format!("{}...{}", prefix, suffix)
}

pub fn print_banner() {
    println!("\n╔═══════════════════════════╗");
    println!("║      Multisig Demo        ║");
    println!("╚═══════════════════════════╝\n");
}

pub fn print_section(title: &str) {
    println!("\n━━━ {} ━━━", title);
}

pub fn print_success(message: &str) {
    println!("✓ {}", message);
}

pub fn print_error(message: &str) {
    println!("✗ Error: {}", message);
}

pub fn print_info(message: &str) {
    println!("ℹ {}", message);
}

pub fn print_account_info(account: &MultisigAccount) {
    print_section("Account Information");
    println!("  Account ID:     {}", &account.id().to_hex());
    println!("  Account Type:   {:?}", account.inner().account_type());
    println!("  Nonce:          {}", account.nonce());
}

pub fn print_storage_overview(account: &MultisigAccount, psm_endpoint: &str) {
    print_section("Storage Overview");

    match account.threshold() {
        Ok(threshold) => {
            let num_cosigners = account.cosigner_commitments().len();
            println!("  Multisig Config: {}-of-{}", threshold, num_cosigners);
        }
        Err(_) => println!("  Multisig Config: Not available"),
    }

    println!("  Cosigner Commitments:");
    for (i, commitment) in account.cosigner_commitments_hex().iter().enumerate() {
        println!("    [{}] {}", i, shorten_hex(commitment));
    }

    match account.procedure_threshold_overrides() {
        Ok(overrides) if !overrides.is_empty() => {
            println!("  Procedure Threshold Overrides:");
            for (procedure, threshold) in overrides {
                println!("    - {} => {}", procedure, threshold);
            }
        }
        _ => {}
    }

    println!("  PSM Endpoint: {}", psm_endpoint);
}

pub fn print_vault(account: &MultisigAccount) {
    print_section("Vault (Account Balance)");

    let vault = account.inner().vault();
    let assets: Vec<Asset> = vault.assets().collect();

    if assets.is_empty() {
        println!("  (empty)");
        print_info("Tip: Consume notes to add assets to your vault before sending transfers.");
        return;
    }

    for (i, asset) in assets.iter().enumerate() {
        match asset {
            Asset::Fungible(fungible) => {
                println!(
                    "  [{}] {} tokens (faucet: {})",
                    i + 1,
                    fungible.amount(),
                    shorten_hex(&fungible.faucet_id().to_hex())
                );
            }
            Asset::NonFungible(nft) => {
                println!(
                    "  [{}] NFT (faucet prefix: {})",
                    i + 1,
                    shorten_hex(&format!("{:?}", nft.faucet_id_prefix()))
                );
            }
        }
    }
}

pub fn print_full_hex(label: &str, hex: &str) {
    println!("{}: {}", label, hex);
}

pub fn print_menu_header() {
    println!("\n┌─────────────────────────────────────────────┐");
    println!("│ Main Menu                                   │");
    println!("└─────────────────────────────────────────────┘");
}

pub fn print_menu_option(key: &str, description: &str, enabled: bool) {
    if enabled {
        println!("  [{}] {}", key, description);
    } else {
        println!("  [{}] {} (disabled)", key, description);
    }
}

pub fn print_waiting(message: &str) {
    println!("\n⏳ {}...", message);
}
