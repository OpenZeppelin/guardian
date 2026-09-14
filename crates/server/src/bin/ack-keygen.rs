use std::path::{Path, PathBuf};
use std::process::ExitCode;

use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::Serializable;
use serde::Serialize;

const FALCON_FILE_NAME: &str = "ack-falcon-secret-key";
const ECDSA_FILE_NAME: &str = "ack-ecdsa-secret-key";
const USAGE: &str = "usage: ack-keygen [--out-dir <dir>]\n\n\
Generates a fresh Guardian ACK identity (Falcon + ECDSA secret keys).\n\
Without --out-dir the keys are printed to stdout as one JSON object.\n\
With --out-dir the keys are written to <dir>/ack-falcon-secret-key and\n\
<dir>/ack-ecdsa-secret-key as owner-only (0600) files, the format the `file`\n\
ACK secret provider reads; existing files are never overwritten.";

#[derive(Serialize)]
struct AckKeys {
    falcon_secret_key: String,
    ecdsa_secret_key: String,
}

impl AckKeys {
    fn generate() -> Self {
        Self {
            falcon_secret_key: hex::encode(FalconSecretKey::new().to_bytes()),
            ecdsa_secret_key: hex::encode(EcdsaSecretKey::new().to_bytes()),
        }
    }

    fn print_json(&self) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|error| error.to_string())?;
        println!("{json}");
        Ok(())
    }

    fn write_to_dir(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let falcon_path = dir.join(FALCON_FILE_NAME);
        let ecdsa_path = dir.join(ECDSA_FILE_NAME);
        for path in [&falcon_path, &ecdsa_path] {
            if path.exists() {
                return Err(format!(
                    "{} already exists; refusing to overwrite an existing Guardian identity",
                    path.display()
                ));
            }
        }
        write_owner_only(&falcon_path, &self.falcon_secret_key)?;
        if let Err(error) = write_owner_only(&ecdsa_path, &self.ecdsa_secret_key) {
            let _ = std::fs::remove_file(&falcon_path);
            return Err(error);
        }
        eprintln!("wrote {}", falcon_path.display());
        eprintln!("wrote {}", ecdsa_path.display());
        Ok(())
    }
}

fn write_owner_only(path: &Path, contents: &str) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    if let Err(error) = std::io::Write::write_all(&mut file, contents.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("cannot write {}: {error}", path.display()));
    }
    Ok(())
}

#[derive(Debug)]
enum Output {
    Stdout,
    Directory(PathBuf),
}

impl Output {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = args.peekable();
        let mut output = Self::Stdout;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out-dir" => {
                    let dir = args.next().ok_or("--out-dir requires a directory")?;
                    output = Self::Directory(PathBuf::from(dir));
                }
                "-h" | "--help" => return Err(USAGE.to_string()),
                other => match other.strip_prefix("--out-dir=") {
                    Some(dir) => output = Self::Directory(PathBuf::from(dir)),
                    None => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
                },
            }
        }
        Ok(output)
    }
}

fn run() -> Result<(), String> {
    let output = Output::parse(std::env::args().skip(1))?;
    let keys = AckKeys::generate();
    match output {
        Output::Stdout => keys.print_json(),
        Output::Directory(dir) => keys.write_to_dir(&dir),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parse_defaults_to_stdout_and_accepts_both_out_dir_forms() {
        assert!(matches!(Output::parse(args(&[])).unwrap(), Output::Stdout));
        assert!(matches!(
            Output::parse(args(&["--out-dir", "keys"])).unwrap(),
            Output::Directory(dir) if dir == Path::new("keys")
        ));
        assert!(matches!(
            Output::parse(args(&["--out-dir=keys"])).unwrap(),
            Output::Directory(dir) if dir == Path::new("keys")
        ));
    }

    #[test]
    fn parse_rejects_missing_value_unknown_flag_and_prints_usage_for_help() {
        assert!(Output::parse(args(&["--out-dir"])).is_err());
        assert!(
            Output::parse(args(&["--bogus"]))
                .unwrap_err()
                .contains("usage:")
        );
        assert!(
            Output::parse(args(&["--help"]))
                .unwrap_err()
                .starts_with("usage:")
        );
    }

    #[test]
    fn write_to_dir_creates_both_files_owner_only_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = AckKeys::generate();
        first.write_to_dir(dir.path()).unwrap();
        let falcon = dir.path().join(FALCON_FILE_NAME);
        let ecdsa = dir.path().join(ECDSA_FILE_NAME);
        assert_eq!(
            std::fs::read_to_string(&falcon).unwrap(),
            first.falcon_secret_key
        );
        assert_eq!(
            std::fs::read_to_string(&ecdsa).unwrap(),
            first.ecdsa_secret_key
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&falcon, &ecdsa] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", path.display());
            }
        }

        let err = AckKeys::generate().write_to_dir(dir.path()).unwrap_err();
        assert!(err.contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(&falcon).unwrap(),
            first.falcon_secret_key
        );
        assert_eq!(
            std::fs::read_to_string(&ecdsa).unwrap(),
            first.ecdsa_secret_key
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_to_dir_removes_the_falcon_file_when_the_ecdsa_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let ecdsa = dir.path().join(ECDSA_FILE_NAME);
        std::os::unix::fs::symlink(dir.path().join("dangling-target"), &ecdsa).unwrap();
        assert!(!ecdsa.exists(), "a dangling symlink passes the pre-check");
        let err = AckKeys::generate().write_to_dir(dir.path()).unwrap_err();
        assert!(err.contains(ECDSA_FILE_NAME), "{err}");
        assert!(!dir.path().join(FALCON_FILE_NAME).exists());
    }
}
