use miden_multisig_client::MultisigAccount;

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

pub fn print_storage_overview(account: &MultisigAccount) {
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

    println!("  PSM Endpoint: {}", account.psm_endpoint());
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
