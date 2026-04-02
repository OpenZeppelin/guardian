# Miden Keystore

A secure filesystem-based keystore for managing Miden Falcon RPO cryptographic keys.

## API

### KeyStore Trait

```rust
pub trait KeyStore {
    fn add_key(&self, key: &SecretKey) -> Result<()>;
    fn get_key(&self, pub_key: Word) -> Result<SecretKey>;
    fn sign(&self, pub_key: Word, message: Word) -> Result<Signature>;
    fn generate_key(&self) -> Result<Word>;
}
```

### FilesystemKeyStore

The main implementation that stores keys on the filesystem.

```rust
impl<R: RngCore + SeedableRng + Send + Sync> FilesystemKeyStore<R> {
    pub fn new(keys_directory: PathBuf) -> Result<Self>
    pub fn with_rng(keys_directory: PathBuf, rng: R) -> Result<Self>
}
```

## Example: Creating a Miden Account

```rust
use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use miden_lib::account::auth::AuthRpoFalcon512Multisig;
use rand_chacha::ChaCha20Rng;

let keystore = FilesystemKeyStore::<ChaCha20Rng>::new("./keys".into())?;

// Generate keys for multisig
let pub_key_1_word = keystore.generate_key()?;
let pub_key_2_word = keystore.generate_key()?;
let pub_key_3_word = keystore.generate_key()?;

let pub_key_1 = PublicKey::new(pub_key_1_word);
let pub_key_2 = PublicKey::new(pub_key_2_word);
let pub_key_3 = PublicKey::new(pub_key_3_word);

let approvers = vec![pub_key_1, pub_key_2, pub_key_3];
let threshold = 2u32;

let multisig = AuthRpoFalcon512Multisig::new(threshold, approvers)?;
```

## Testing

```bash
cargo test --package miden-keystore
```

## License

See the main project LICENSE file.
