# PSM Interactive Demo

An interactive CLI demo for Private State Manager (PSM) with Miden multisig accounts. This demo allows multiple users to collaborate in real-time by running the program in separate terminals.

## Features

- **Multi-terminal Support**: Each participant runs their own instance to simulate real multi-party scenarios
- **Interactive Menu**: User-friendly CLI with command history
- **Multisig Workflows**: Create accounts, add cosigners, coordinate signatures
- **PSM Integration**: Configure accounts in PSM, pull state, push deltas, coordinate proposals
- **Shortened Display**: Public keys shown in compact format (e.g., `0xABCD...WXYZ`) for better UX

## Prerequisites

- **PSM Server** running on `http://localhost:50051` (or custom endpoint)
- **Miden Node** running on `http://localhost:57291` (or custom endpoint)

## Quick Start

### Start Required Services

1. **Start PSM Server**:
   ```bash
   cargo run --package private-state-manager-server --bin server
   ```

2. **Start Miden Node**:
   Follow the [Miden node setup instructions](https://docs.polygon.technology/miden/) to run a local node on port 57291.

### Run the Demo

```bash
cd examples/demo-interactive
cargo run
```

The program will prompt for:
- PSM Server endpoint (default: `http://localhost:50051`)
- Miden Node endpoint (default: `http://localhost:57291`)

## Usage Flow

### Terminal 1 (Account Creator)

1. **Generate Keypair** - Creates your Falcon keypair and shows full commitment
2. **Create Multisig Account** - Creates a 2-of-2 (or N-of-M) multisig account
   - Enter threshold (e.g., `2`)
   - Enter number of cosigners (e.g., `2`)
   - Your commitment is shown automatically
   - Enter commitment from Terminal 2
3. **Configure Account in PSM** - Pushes initial account state to PSM
4. **Show Account Details** - View account ID, storage slots, and configuration

### Terminal 2 (Cosigner)

1. **Generate Keypair** - Creates keypair and saves full commitment to share
2. **Pull Account from PSM** - Fetches the account created by Terminal 1
   - Enter the account ID shown in Terminal 1
3. **Add Cosigner** - (Future) Update multisig to 3-of-3 by adding another party
4. **Sign Proposal** - (Future) Review and sign delta proposals created by other cosigners

## Menu Options

- `[1]` Generate keypair - Creates Falcon keypair (only available once)
- `[2]` Create multisig account - Creates new account with PSM auth
- `[3]` Configure account in PSM - Pushes account to PSM server
- `[4]` Pull account from PSM - Fetches account state from PSM
- `[5]` Add cosigner - Updates multisig to N+1 configuration
- `[6]` Create delta proposal - Create a proposal for other cosigners to review and sign
- `[7]` Sign delta proposal - Add your signature to an existing proposal
- `[8]` List delta proposals - View pending proposals for the account
- `[s]` Show account details - Displays full account information
- `[c]` Show connection status - Shows PSM/Miden connection status
- `[q]` Quit - Exit the program

## Account Storage Layout

The multisig account uses 6 storage slots:

- **Slot 0**: Multisig config (threshold, num_cosigners)
- **Slot 1**: Cosigner public keys (map)
- **Slot 2**: Executed transactions (map)
- **Slot 3**: Procedure thresholds (map)
- **Slot 4**: PSM selector (value: 1 = enabled)
- **Slot 5**: PSM server public key

## Tips

- **Save Commitments**: Copy the full commitment hex when generating keypairs - you'll need it for account creation
- **Account IDs**: When sharing account IDs between terminals, use the full hex string
- **Connection Issues**: Ensure PSM server and Miden node are running before starting the demo
- **Fresh Start**: Each run creates a new temporary directory for Miden client state

## Troubleshooting

### "Failed to connect to PSM"
- Verify PSM server is running: `curl http://localhost:50051`
- Check the endpoint you entered at startup

### "Failed to connect to Miden node"
- Verify Miden node is running on the correct port
- Check the endpoint format (must include `http://` or `https://`)

### "PSM configuration failed"
- Ensure you generated a keypair first
- Ensure the account was created successfully
- Check PSM server logs for detailed error messages

## Architecture

The demo is structured as a standalone Cargo project:

- **state.rs** - Session state management (connections, account, keys)
- **display.rs** - UI formatting and display utilities
- **menu.rs** - Interactive menu system and input handling
- **actions.rs** - Menu action handlers (create, configure, sign, etc.)
- **helpers.rs** - Utility functions (Miden client, Word formatting)
- **falcon.rs** - Falcon keypair generation
- **multisig.rs** - Multisig account creation and transaction building
- **main.rs** - Entry point and main loop

## Future Enhancements

- Complete "Add Cosigner" workflow with transaction simulation and PSM coordination
- Implement delta proposal workflows for multi-party coordination
- Support for transaction nonce management
- Delta merging and conflict resolution
- Account state visualization
