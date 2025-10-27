import { WebClient, type Auth, type ConfigureRequest } from "./client.js";
import {
  AccountBuilder,
  AccountComponent,
  AccountStorageMode,
  AccountType,
  SecretKey,
  Word,
  Felt,
  PublicKey,
  TransactionKernel,
  StorageSlot,
  StorageMap,
  WebClient as SdkWebClient,
  TransactionRequestBuilder,
} from "@demox-labs/miden-sdk";

const output = document.getElementById("output")!;
const runBtn = document.getElementById("runBtn") as HTMLButtonElement;
const clearBtn = document.getElementById("clearBtn") as HTMLButtonElement;
const endpointInput = document.getElementById("endpoint") as HTMLInputElement;
const midenRpcInput = document.getElementById("midenRpc") as HTMLInputElement;

function log(message: string, type: "info" | "success" | "error" | "warning" = "info") {
  const span = document.createElement("span");
  span.className = type;
  span.textContent = message + "\n";
  output.appendChild(span);
  output.scrollTop = output.scrollHeight;
}

function clear() {
  output.innerHTML = "";
}

function wordToHex(word: Word): string {
  const bytes = word.serialize();
  return "0x" + Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function feltToU64(felt: Felt): bigint {
  return felt.asInt();
}

async function verifyCommitmentSignature(
  commitment: string,
  pubkeyHex: string,
  signatureHex: string
): Promise<boolean> {
  try {
    const hexToWord = (hex: string): Word => {
      const cleanHex = hex.startsWith("0x") ? hex.slice(2) : hex;
      const bytes = new Uint8Array(cleanHex.match(/.{2}/g)!.map((b) => parseInt(b, 16)));
      return Word.deserialize(bytes);
    };

    const commitmentWord = hexToWord(commitment);
    const { Rpo256, FeltArray } = await import("@demox-labs/miden-sdk");
    const commitmentFelts = commitmentWord.toFelts();
    const feltArray = new FeltArray(commitmentFelts);
    const digest = Rpo256.hashElements(feltArray);

    const pubkeyBytes = new Uint8Array(
      pubkeyHex
        .replace("0x", "")
        .match(/.{2}/g)!
        .map((b) => parseInt(b, 16))
    );
    const signatureBytes = new Uint8Array(
      signatureHex
        .replace("0x", "")
        .match(/.{2}/g)!
        .map((b) => parseInt(b, 16))
    );

    const { Signature } = await import("@demox-labs/miden-sdk");
    const publicKey = PublicKey.deserialize(pubkeyBytes);
    const signature = Signature.deserialize(signatureBytes);

    return publicKey.verify(digest, signature);
  } catch (e) {
    log(`Signature verification error: ${e}`, "error");
    return false;
  }
}

async function runE2E() {
  clear();
  runBtn.disabled = true;

  try {
    log("=== Private State Manager E2E Test ===\n", "info");

    const psmEndpoint = endpointInput.value;
    const midenRpcUrl = midenRpcInput.value;

    log("1. Generating keys and creating account...", "info");

    const secretKey1 = SecretKey.withRng();
    const secretKey2 = SecretKey.withRng();
    const secretKey3 = SecretKey.withRng();

    const pubKey1 = secretKey1.publicKey();
    const pubKey2 = secretKey2.publicKey();
    const pubKey3 = secretKey3.publicKey();

    const pubKey1Bytes = pubKey1.serialize();
    const pubKey2Bytes = pubKey2.serialize();
    const pubKey3Bytes = pubKey3.serialize();

    const pubKey1Word = Word.deserialize(pubKey1Bytes.slice(0, 32));
    const pubKey2Word = Word.deserialize(pubKey2Bytes.slice(0, 32));
    const pubKey3Word = Word.deserialize(pubKey3Bytes.slice(0, 32));

    const threshold = 2;

    const authComponent = AccountComponent.createAuthComponent(secretKey1);

    const storageMap = new StorageMap();
    storageMap.insert(Word.newFromFelts([new Felt(0n), new Felt(0n), new Felt(0n), new Felt(0n)]), pubKey1Word);
    storageMap.insert(Word.newFromFelts([new Felt(1n), new Felt(0n), new Felt(0n), new Felt(0n)]), pubKey2Word);
    storageMap.insert(Word.newFromFelts([new Felt(2n), new Felt(0n), new Felt(0n), new Felt(0n)]), pubKey3Word);

    const slot1 = StorageSlot.map(storageMap);

    const slot0Value = Word.newFromFelts([
      new Felt(BigInt(threshold)),
      new Felt(3n),
      new Felt(0n),
      new Felt(0n),
    ]);
    const slot0 = StorageSlot.fromValue(slot0Value);

    const assembler = TransactionKernel.assembler();
    const walletSource = `
export.wallet_proc
    push.1
    drop
end
    `;
    const walletComponent = AccountComponent.compile(walletSource, assembler, [slot0, slot1])
      .withSupportsAllTypes();

    const initSeed = new Uint8Array(32);
    for (let i = 0; i < 32; i++) initSeed[i] = 0xff;

    const accountBuilder = new AccountBuilder(initSeed)
      .accountType(AccountType.RegularAccountUpdatableCode)
      .storageMode(AccountStorageMode.private())
      .withAuthComponent(authComponent)
      .withComponent(walletComponent);

    const builderResult = accountBuilder.build();
    const account = builderResult.account;
    const accountId = account.id();
    const initialCommitment = account.commitment();
    const initialNonce = account.nonce();

    log(`  Account ID: ${accountId.toString()}`, "success");
    log(`  Initial Commitment: ${wordToHex(initialCommitment)}`, "success");
    log(`  Threshold: ${threshold}/3`, "success");
    log("");

    log("2. Preparing PSM client with auth...", "info");

    const pubkey1Hex = wordToHex(pubKey1Word);
    const pubkey2Hex = wordToHex(pubKey2Word);
    const pubkey3Hex = wordToHex(pubKey3Word);

    const accountIdPrefix = accountId.prefix();
    const accountIdSuffix = accountId.suffix();

    const messageElements = [
      accountIdPrefix,
      accountIdSuffix,
      new Felt(0n),
      new Felt(0n),
    ];

    const { Rpo256, FeltArray } = await import("@demox-labs/miden-sdk");
    const feltArray = new FeltArray(messageElements);
    const messageDigest = Rpo256.hashElements(feltArray);

    const signature = secretKey1.sign(messageDigest);
    const signatureHex =
      "0x" + Array.from(signature.serialize(), (b) => b.toString(16).padStart(2, "0")).join("");

    const auth: Auth = {
      pubkey: pubkey1Hex,
      signature: signatureHex,
    };

    log("  ✓ Auth prepared", "success");
    log("");

    log("3. Configuring account on PSM...", "info");
    const client = new WebClient(psmEndpoint);

    // Serialize account to base64 (matching server's ToJson format)
    const accountBytes = account.serialize();
    const accountBase64 = btoa(String.fromCharCode(...accountBytes));
    const accountJson = {
      data: accountBase64,
      account_id: accountId.toString(),
    };

    const configRequest: ConfigureRequest = {
      account_id: accountId.toString(),
      auth: {
        MidenFalconRpo: {
          cosigner_pubkeys: [pubkey1Hex, pubkey2Hex, pubkey3Hex],
        },
      },
      initial_state: accountJson,
      storage_type: "Filesystem",
    };

    const configResponse = await client.configure(auth, configRequest);
    const serverAckPubkey = configResponse.ack_pubkey!;
    log(`  ✓ ${configResponse.message}`, "success");
    log(`  Server ack pubkey: ${serverAckPubkey.substring(0, 20)}...`, "success");
    log("");

    log("4. Executing transaction on Miden testnet...", "info");
    log("  Connecting to Miden RPC...", "info");

    const sdkClient = await SdkWebClient.createClient(midenRpcUrl);

    // Import the account into the client
    await sdkClient.newAccount(account, builderResult.seed, false);
    log("  ✓ Account imported to client", "success");

    // Add the secret key to the client (required for transaction signing)
    await sdkClient.addAccountSecretKeyToWebStore(secretKey1);
    log("  ✓ Secret key added for authentication", "success");

    // Sync state with the network
    await sdkClient.syncState();
    log("  ✓ Synced with testnet", "success");

    // Execute a minimal transaction (nonce increment only)
    // Note: Custom storage modifications require auth procedure execution
    // which is complex in the browser environment
    log("  Executing transaction (nonce increment)...", "info");
    const txRequest = new TransactionRequestBuilder().build();
    const txResult = await sdkClient.newTransaction(accountId, txRequest);
    log(`  ✓ Transaction executed`, "success");

    // Get the account delta from the transaction result
    const accountDelta = txResult.accountDelta();
    const deltaBytes = accountDelta.serialize();

    // Serialize delta to base64 (matching server's ToJson format)
    const deltaBase64 = btoa(String.fromCharCode(...deltaBytes));
    const deltaObj = {
      data: deltaBase64,
    };

    // Get the final account state
    const executedTx = txResult.executedTransaction();
    const finalAccountHeader = executedTx.finalAccount();
    const newCommitment = finalAccountHeader.commitment();
    const newCommitmentHex = wordToHex(newCommitment);
    const newNonce = finalAccountHeader.nonce();

    log(`  New commitment: ${newCommitmentHex}`, "success");
    log(`  New nonce: ${feltToU64(newNonce)}`, "success");
    log("");

    log("5. Pushing delta to PSM...", "info");
    const prevCommitmentHex = wordToHex(initialCommitment);

    const deltaResponse = await client.pushDelta(auth, {
      account_id: accountId.toString(),
      nonce: Number(feltToU64(newNonce)),
      prev_commitment: prevCommitmentHex,
      delta_payload: deltaObj,
    });

    log(`  ✓ Delta pushed successfully`, "success");
    log(`  New commitment: ${deltaResponse.new_commitment}`, "success");
    log(`  Server signature: ${deltaResponse.ack_sig?.substring(0, 20)}...`, "success");
    log("");

    log("6. Verifying server signature...", "info");
    const isValid = await verifyCommitmentSignature(
      deltaResponse.new_commitment!,
      serverAckPubkey,
      deltaResponse.ack_sig!
    );
    log(`  ${isValid ? "✓" : "✗"} Server signature is ${isValid ? "VALID" : "INVALID"}`, isValid ? "success" : "error");
    log("");

    log("7. Retrieving delta from PSM...", "info");
    const retrievedDelta = await client.getDelta(auth, accountId.toString(), Number(feltToU64(newNonce)));
    log(`  ✓ Retrieved delta with nonce ${retrievedDelta.nonce}`, "success");
    log(`  Commitment: ${retrievedDelta.new_commitment}`, "success");
    log("");

    log("8. Getting account state from PSM...", "info");
    const state = await client.getState(auth, accountId.toString());
    log(`  ✓ Retrieved state`, "success");
    log(`  Commitment: ${state.commitment}`, "success");
    log(`  Updated at: ${state.updated_at}`, "success");
    log("");

    log("=== E2E Test Completed Successfully! ===", "success");
  } catch (error: any) {
    log(`\n✗ Error: ${error.message || error}`, "error");
    console.error(error);
  } finally {
    runBtn.disabled = false;
  }
}

clearBtn.addEventListener("click", clear);
runBtn.addEventListener("click", runE2E);

log("Ready to run E2E test. Click 'Run E2E Test' to begin.", "info");
