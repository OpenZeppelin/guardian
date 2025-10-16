# Private State Manager

Warning: This is a work in progress.

### Project Structure

- **[crates/server](crates/server/README.md)** - Server for managing private account states and deltas
  - Reproducible builds for binary verification and TEE deployment
- **crates/client** - Client side SDK
- **crates/shared** - Shared types and utilities

### Quick Start

See the [Server README](crates/server/README.md) for detailed API documentation and usage examples.

### Configuration

#### Environment Variables

- `PSM_APP_PATH` - Base directory for PSM writes (default: `/var/psm/app`)
- `PSM_ENV` - Environment (default: `dev`)
- `RUST_LOG` - Logging level (default: `info`)

### Running

#### Running with Docker Compose

1. Copy `.env.example` to `.env`

```bash
cp .env.example .env
```

2. Edit `.env` with your configuration

3. Start the server:

```bash
docker-compose up --build -d
```

4. View logs:

```bash
docker-compose logs -f
```

5. Stop services:

```bash
docker-compose down
```

The server will be available at `http://localhost:3000`

### Local Storage structure

```
/var/psm/
├── app/                           # Write directory (PSM only)
│   ├── .metadata/
│   │   └── accounts.json         # Account metadata (storage type, cosigner pubkeys, timestamps)
│   └── {account_id}/
│       ├── state.json            # Current account state
│       └── deltas/
│           └── {nonce}.json      # Individual deltas
```
