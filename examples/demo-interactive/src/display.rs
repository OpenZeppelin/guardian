use miden_client::account::Account;
use miden_client::Word;

use crate::helpers::format_word_as_hex;

pub fn shorten_hex(hex: &str) -> String {
    if hex.len() <= 12 {
        return hex.to_string();
    }

    let prefix = &hex[..6];
    let suffix = &hex[hex.len() - 4..];
    format!("{}...{}", prefix, suffix)
}

pub fn print_banner() {
    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║     Private State Manager - Interactive Demo             ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");
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

pub fn print_account_info(account: &Account) {
    print_section("Account Information");
    println!("  Account ID:     {}", shorten_hex(&account.id().to_string()));
    println!("  Account Type:   {:?}", account.account_type());
    println!("  Nonce:          {}", account.nonce());
}

pub fn print_storage_slot(index: usize, word: &Word, description: &str) {
    let hex = format_word_as_hex(word);
    println!("  Slot {}: {} - {}", index, shorten_hex(&hex), description);
}

pub fn print_storage_overview(account: &Account) {
    print_section("Storage Overview");

    let storage = account.storage();

    match storage.get_item(0) {
        Ok(word) => {
            let threshold = word[0].as_int();
            let num_cosigners = word[1].as_int();
            println!("  Slot 0: Multisig Config ({}-of-{})", threshold, num_cosigners);
        }
        Err(_) => println!("  Slot 0: Not set"),
    }

    match storage.get_item(1) {
        Ok(_) => println!("  Slot 1: Cosigner Public Keys (map)"),
        Err(_) => println!("  Slot 1: Not set"),
    }

    println!("  Slot 2: Executed Transactions (map)");
    println!("  Slot 3: Procedure Thresholds (map)");

    match storage.get_item(4) {
        Ok(word) => {
            let selector = word[0].as_int();
            println!("  Slot 4: PSM Selector (value: {})", selector);
        }
        Err(_) => println!("  Slot 4: Not set"),
    }

    match storage.get_item(5) {
        Ok(word) => {
            let hex = format_word_as_hex(&word);
            println!("  Slot 5: PSM Public Key ({})", shorten_hex(&hex));
        }
        Err(_) => println!("  Slot 5: Not set"),
    }
}

pub fn print_full_hex(label: &str, hex: &str) {
    println!("{}: {}", label, hex);
}

pub fn print_connection_status(psm_connected: bool, miden_connected: bool) {
    print_section("Connection Status");

    let psm_status = if psm_connected { "✓ Connected" } else { "✗ Not connected" };
    let miden_status = if miden_connected { "✓ Connected" } else { "✗ Not connected" };

    println!("  PSM Server:   {}", psm_status);
    println!("  Miden Node:   {}", miden_status);
}

pub fn print_keypair_generated(pubkey_hex: &str, commitment_hex: &str) {
    print_section("Keypair Generated");
    print_full_hex("  Public Key", pubkey_hex);
    print_full_hex("  Commitment", commitment_hex);
    println!("\n  Note: Save these values for later reference");
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
