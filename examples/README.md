# Examples

Example implementations showing how to use the Private State Manager (PSM).

## Available Examples

### [Rust Example](./rust/)
Command-line client demonstrating PSM integration in Rust.

**Run:**
```bash
cd rust
cargo run --example e2e
```

### [Web Example](./web/)
Browser-based client using TypeScript and the Miden SDK.

**Run:**
```bash
cd web
npm install
npm run dev
```

NOTE: Before running any example, you need to start the PSM server.

```bash
cargo run --package private-state-manager-server --bin server
```
