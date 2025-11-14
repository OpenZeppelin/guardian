#[cfg(feature = "e2e")]
mod fixtures {
    use miden_client::account::component::{AccountComponent, BasicWallet};
    use miden_client::account::{
        Account, AccountBuilder, AccountStorageMode, AccountType, StorageMap, StorageSlot,
    };
    use miden_client::transaction::TransactionKernel;
    use miden_client::{Deserializable, Serializable, Word};
    use miden_objects::account::AccountDelta;
    use miden_objects::account::delta::{AccountStorageDelta, AccountVaultDelta};
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
    use miden_objects::transaction::{InputNotes, OutputNotes, TransactionSummary};
    use miden_objects::{Felt, Word as MidenWord, ZERO};
    use private_state_manager_shared::{FromJson, ToJson};
    use std::fs;

    const MULTISIG_PSM_AUTH: &str = include_str!("fixtures/multisig-psm.masm");

    fn create_multisig_psm_account(
        threshold: u64,
        cosigner_commitments: &[&str],
        psm_server_pubkey_hex: &str,
        init_seed: [u8; 32],
    ) -> Account {
        let psm_pubkey_bytes =
            hex::decode(&psm_server_pubkey_hex[2..]).expect("Failed to decode PSM pubkey");
        let psm_commitment_word = Word::read_from_bytes(&psm_pubkey_bytes)
            .expect("Failed to convert PSM commitment to Word");

        let num_cosigners = cosigner_commitments.len() as u64;

        let slot_0 = StorageSlot::Value(Word::from([threshold as u32, num_cosigners as u32, 0, 0]));

        let mut client_pubkeys_map = StorageMap::new();
        for (i, commitment_hex) in cosigner_commitments.iter().enumerate() {
            let pubkey_bytes = hex::decode(&commitment_hex[2..])
                .unwrap_or_else(|_| panic!("Failed to decode cosigner {} pubkey", i));
            let commitment_word = Word::read_from_bytes(&pubkey_bytes)
                .unwrap_or_else(|_| panic!("Failed to convert cosigner {} commitment to Word", i));

            let _ = client_pubkeys_map.insert(Word::from([i as u32, 0, 0, 0]), commitment_word);
        }
        let slot_1 = StorageSlot::Map(client_pubkeys_map);

        let slot_2 = StorageSlot::Map(StorageMap::new());

        let mut proc_thresholds_map = StorageMap::new();
        proc_thresholds_map
            .insert(
                Word::from([u32::MAX, u32::MAX, u32::MAX, u32::MAX]),
                Word::from([1u32, 0, 0, 0]),
            )
            .expect("procedure threshold sentinel");
        let slot_3 = StorageSlot::Map(proc_thresholds_map);
        let slot_4 = StorageSlot::Value(Word::from([1u32, 0, 0, 0]));

        let mut psm_key_map = StorageMap::new();
        let _ = psm_key_map.insert(Word::from([0u32, 0, 0, 0]), psm_commitment_word);
        let slot_5 = StorageSlot::Map(psm_key_map);

        let auth_component = AccountComponent::compile(
            MULTISIG_PSM_AUTH.to_string(),
            TransactionKernel::assembler(),
            vec![slot_0, slot_1, slot_2, slot_3, slot_4, slot_5],
        )
        .expect("Failed to compile multisig+PSM auth component")
        .with_supports_all_types();

        AccountBuilder::new(init_seed)
            .account_type(AccountType::RegularAccountUpdatableCode)
            .storage_mode(AccountStorageMode::Public)
            .with_auth_component(auth_component)
            .with_component(BasicWallet)
            .build()
            .expect("Failed to build account")
    }

    #[tokio::test]
    #[ignore]
    async fn generate_multisig_fixtures() {
        println!("\n🔧 Generating Multisig PSM Fixtures...\n");

        let secret_key_1 = SecretKey::new();
        let secret_key_2 = SecretKey::new();
        let secret_key_3 = SecretKey::new();

        let pub_key_1 = secret_key_1.public_key();
        let pub_key_2 = secret_key_2.public_key();
        let pub_key_3 = secret_key_3.public_key();

        let commitment_1 = pub_key_1.to_commitment();
        let commitment_2 = pub_key_2.to_commitment();
        let commitment_3 = pub_key_3.to_commitment();

        let commitment_1_hex = format!("0x{}", hex::encode(commitment_1.as_bytes()));
        let commitment_2_hex = format!("0x{}", hex::encode(commitment_2.as_bytes()));
        let commitment_3_hex = format!("0x{}", hex::encode(commitment_3.as_bytes()));

        let psm_secret_key = SecretKey::new();
        let psm_pubkey = psm_secret_key.public_key();
        let psm_commitment = psm_pubkey.to_commitment();
        let psm_commitment_hex = format!("0x{}", hex::encode(psm_commitment.as_bytes()));

        println!("Generated Keys:");
        println!("  PSM:     {}", psm_commitment_hex);
        println!("  Signer 1: {}", commitment_1_hex);
        println!("  Signer 2: {}", commitment_2_hex);
        println!("  Signer 3: {}", commitment_3_hex);

        let threshold = 2u64;
        let cosigner_refs = vec![
            commitment_1_hex.as_str(),
            commitment_2_hex.as_str(),
            commitment_3_hex.as_str(),
        ];

        let account =
            create_multisig_psm_account(threshold, &cosigner_refs, &psm_commitment_hex, [0xff; 32]);

        let account_json = account.to_json();
        let account_id = account.id();
        let mut current_commitment = account.commitment();

        println!("\n📦 Generated Multisig Account:");
        println!("  Account ID: {}", account_id);
        println!(
            "  Commitment: 0x{}",
            hex::encode(current_commitment.as_bytes())
        );
        println!(
            "  Config: {}/{} multisig + PSM",
            threshold,
            cosigner_refs.len()
        );

        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("testing")
            .join("fixtures")
            .join("account.json");

        fs::write(
            &fixture_path,
            serde_json::to_string_pretty(&account_json).unwrap(),
        )
        .expect("Failed to write account.json");

        println!("✅ Saved account.json");

        let mut commitments = vec![(
            "initial_commitment".to_string(),
            format!("0x{}", hex::encode(current_commitment.as_bytes())),
        )];

        let secret_key_4 = SecretKey::new();
        let pub_key_4 = secret_key_4.public_key();
        let commitment_4 = pub_key_4.to_commitment();
        let commitment_4_hex = format!("0x{}", hex::encode(commitment_4.as_bytes()));

        println!("\n🔄 Delta 1: Add 4th signer");
        println!("  New signer: {}", commitment_4_hex);

        let mut storage_delta_1 = AccountStorageDelta::default();
        storage_delta_1.set_map_item(
            1,
            MidenWord::from([Felt::new(3), ZERO, ZERO, ZERO]),
            commitment_4,
        );
        storage_delta_1.set_item(
            0,
            MidenWord::from([Felt::new(threshold), Felt::new(4), ZERO, ZERO]),
        );

        let delta_1 = AccountDelta::new(
            account_id,
            storage_delta_1,
            AccountVaultDelta::default(),
            Felt::new(1),
        )
        .expect("Failed to create delta 1");

        let mut account_state: Account =
            Account::from_json(&account_json).expect("Failed to deserialize");
        let prev_commitment_1 = current_commitment;
        account_state
            .apply_delta(&delta_1)
            .expect("Failed to apply delta 1");
        current_commitment = account_state.commitment();

        println!(
            "  New commitment: 0x{}",
            hex::encode(current_commitment.as_bytes())
        );

        let tx_summary_1 = TransactionSummary::new(
            delta_1,
            InputNotes::new(Vec::new()).unwrap(),
            OutputNotes::new(Vec::new()).unwrap(),
            MidenWord::from([ZERO; 4]),
        );

        let delta_1_fixture = serde_json::json!({
            "account_id": format!("{}", account_id),
            "nonce": 1,
            "prev_commitment": format!("0x{}", hex::encode(prev_commitment_1.as_bytes())),
            "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
            "delta_payload": tx_summary_1.to_json()
        });

        fs::write(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testing")
                .join("fixtures")
                .join("delta_1.json"),
            serde_json::to_string_pretty(&delta_1_fixture).unwrap(),
        )
        .expect("Failed to write delta_1.json");

        commitments.push((
            "commitment_after_delta_1".to_string(),
            format!("0x{}", hex::encode(current_commitment.as_bytes())),
        ));

        println!("✅ Saved delta_1.json");

        let secret_key_5 = SecretKey::new();
        let pub_key_5 = secret_key_5.public_key();
        let commitment_5 = pub_key_5.to_commitment();
        let commitment_5_hex = format!("0x{}", hex::encode(commitment_5.as_bytes()));

        println!("\n🔄 Delta 2: Add 5th signer");
        println!("  New signer: {}", commitment_5_hex);

        let mut storage_delta_2 = AccountStorageDelta::default();
        storage_delta_2.set_map_item(
            1,
            MidenWord::from([Felt::new(4), ZERO, ZERO, ZERO]),
            commitment_5,
        );
        storage_delta_2.set_item(
            0,
            MidenWord::from([Felt::new(threshold), Felt::new(5), ZERO, ZERO]),
        );

        let delta_2 = AccountDelta::new(
            account_id,
            storage_delta_2,
            AccountVaultDelta::default(),
            Felt::new(1),
        )
        .expect("Failed to create delta 2");

        let prev_commitment_2 = current_commitment;
        account_state
            .apply_delta(&delta_2)
            .expect("Failed to apply delta 2");
        current_commitment = account_state.commitment();

        println!(
            "  New commitment: 0x{}",
            hex::encode(current_commitment.as_bytes())
        );

        let tx_summary_2 = TransactionSummary::new(
            delta_2,
            InputNotes::new(Vec::new()).unwrap(),
            OutputNotes::new(Vec::new()).unwrap(),
            MidenWord::from([ZERO; 4]),
        );

        let delta_2_fixture = serde_json::json!({
            "account_id": format!("{}", account_id),
            "nonce": 2,
            "prev_commitment": format!("0x{}", hex::encode(prev_commitment_2.as_bytes())),
            "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
            "delta_payload": tx_summary_2.to_json()
        });

        fs::write(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testing")
                .join("fixtures")
                .join("delta_2.json"),
            serde_json::to_string_pretty(&delta_2_fixture).unwrap(),
        )
        .expect("Failed to write delta_2.json");

        commitments.push((
            "commitment_after_delta_2".to_string(),
            format!("0x{}", hex::encode(current_commitment.as_bytes())),
        ));

        println!("✅ Saved delta_2.json");

        println!("\n🔄 Delta 3: Increase threshold to 3");

        let mut storage_delta_3 = AccountStorageDelta::default();
        storage_delta_3.set_item(0, MidenWord::from([Felt::new(3), Felt::new(5), ZERO, ZERO]));

        let delta_3 = AccountDelta::new(
            account_id,
            storage_delta_3,
            AccountVaultDelta::default(),
            Felt::new(1),
        )
        .expect("Failed to create delta 3");

        let prev_commitment_3 = current_commitment;
        account_state
            .apply_delta(&delta_3)
            .expect("Failed to apply delta 3");
        current_commitment = account_state.commitment();

        println!("  New threshold: 3/5");
        println!(
            "  New commitment: 0x{}",
            hex::encode(current_commitment.as_bytes())
        );

        let tx_summary_3 = TransactionSummary::new(
            delta_3,
            InputNotes::new(Vec::new()).unwrap(),
            OutputNotes::new(Vec::new()).unwrap(),
            MidenWord::from([ZERO; 4]),
        );

        let delta_3_fixture = serde_json::json!({
            "account_id": format!("{}", account_id),
            "nonce": 3,
            "prev_commitment": format!("0x{}", hex::encode(prev_commitment_3.as_bytes())),
            "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
            "delta_payload": tx_summary_3.to_json()
        });

        fs::write(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testing")
                .join("fixtures")
                .join("delta_3.json"),
            serde_json::to_string_pretty(&delta_3_fixture).unwrap(),
        )
        .expect("Failed to write delta_3.json");

        commitments.push((
            "commitment_after_delta_3".to_string(),
            format!("0x{}", hex::encode(current_commitment.as_bytes())),
        ));

        println!("✅ Saved delta_3.json");

        let mut commitments_map = serde_json::Map::new();
        commitments_map.insert(
            "account_id".to_string(),
            serde_json::json!(format!("{}", account_id)),
        );
        for (key, value) in commitments {
            commitments_map.insert(key, serde_json::json!(value));
        }

        fs::write(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testing")
                .join("fixtures")
                .join("commitments.json"),
            serde_json::to_string_pretty(&commitments_map).unwrap(),
        )
        .expect("Failed to write commitments.json");

        println!("✅ Saved commitments.json");

        let keys_fixture = serde_json::json!({
            "psm_secret_key": hex::encode(psm_secret_key.to_bytes()),
            "psm_commitment": psm_commitment_hex,
            "signer_1_secret_key": hex::encode(secret_key_1.to_bytes()),
            "signer_1_commitment": commitment_1_hex,
            "signer_2_secret_key": hex::encode(secret_key_2.to_bytes()),
            "signer_2_commitment": commitment_2_hex,
            "signer_3_secret_key": hex::encode(secret_key_3.to_bytes()),
            "signer_3_commitment": commitment_3_hex,
            "signer_4_secret_key": hex::encode(secret_key_4.to_bytes()),
            "signer_4_commitment": commitment_4_hex,
            "signer_5_secret_key": hex::encode(secret_key_5.to_bytes()),
            "signer_5_commitment": commitment_5_hex,
        });

        fs::write(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testing")
                .join("fixtures")
                .join("keys.json"),
            serde_json::to_string_pretty(&keys_fixture).unwrap(),
        )
        .expect("Failed to write keys.json");

        println!("✅ Saved keys.json");

        println!("\n🎉 All fixtures generated successfully!");
        println!("\nGenerated files:");
        println!(
            "  ✓ account.json (initial: {}/{} + PSM)",
            threshold,
            cosigner_refs.len()
        );
        println!("  ✓ delta_1.json (add 4th signer)");
        println!("  ✓ delta_2.json (add 5th signer)");
        println!("  ✓ delta_3.json (increase threshold to 3)");
        println!("  ✓ commitments.json (commitment history)");
        println!("  ✓ keys.json (secret keys for testing)");
    }
}
