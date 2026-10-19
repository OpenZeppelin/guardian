//! Operator CLI for the policy module endpoints.
//!
//! ```text
//! modules_admin keygen <key-file>
//! modules_admin [--url http://localhost:3000] --key <key-file> GET  /modules
//! modules_admin [--url ...] --key <key-file> PUT  /modules/blocklist/params '{"params":{...},"reason":"..."}'
//! ```

use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};

type Error = Box<dyn std::error::Error>;

fn load_key(path: &str) -> Result<SecretKey, Error> {
    let bytes = hex::decode(std::fs::read_to_string(path)?.trim())?;
    Ok(SecretKey::read_from_bytes(&bytes)?)
}

fn keygen(path: &str) -> Result<(), Error> {
    let key = SecretKey::new();
    std::fs::write(path, hex::encode(key.to_bytes()))?;
    let public_key = key.public_key();
    println!("key written to {path}");
    println!("commitment: {}", public_key.to_commitment().into_hex());
    println!("allowlist entry (put it in the GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE array):");
    println!(
        "{}",
        serde_json::json!({
            "public_key": public_key.into_hex(),
            "permissions": ["dashboard:read", "policies:write", "policies:write_global"]
        })
    );
    Ok(())
}

async fn login(http: &reqwest::Client, url: &str, key: &SecretKey) -> Result<String, Error> {
    let commitment = key.public_key().to_commitment().into_hex();
    let challenge: serde_json::Value = http
        .get(format!("{url}/auth/challenge"))
        .query(&[("commitment", &commitment)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let digest = challenge["challenge"]["signing_digest"]
        .as_str()
        .ok_or("challenge has no signing_digest")?;
    let signature = key.sign(Word::from_hex(digest)?);
    let response = http
        .post(format!("{url}/auth/verify"))
        .json(&serde_json::json!({
            "commitment": commitment,
            "signature": format!("0x{}", hex::encode(signature.to_bytes())),
        }))
        .send()
        .await?;
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::to_string)
        .ok_or("login failed: no session cookie (is the key in the operator allowlist?)")?;
    response.error_for_status()?;
    Ok(cookie)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("keygen") {
        return keygen(args.get(1).ok_or("keygen needs a key file path")?);
    }
    let mut url = "http://localhost:3000".to_string();
    let mut key_path = None;
    while args.first().is_some_and(|arg| arg.starts_with("--")) {
        let flag = args.remove(0);
        let value = (!args.is_empty())
            .then(|| args.remove(0))
            .ok_or("flag needs a value")?;
        match flag.as_str() {
            "--url" => url = value,
            "--key" => key_path = Some(value),
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    let [method, path, rest @ ..] = args.as_slice() else {
        return Err("usage: modules_admin --key <file> <METHOD> <PATH> [JSON]".into());
    };
    let key = load_key(key_path.as_deref().ok_or("--key is required")?)?;
    let http = reqwest::Client::new();
    let cookie = login(&http, &url, &key).await?;

    let request = http
        .request(method.parse()?, format!("{url}/dashboard{path}"))
        .header(reqwest::header::COOKIE, cookie);
    let request = match rest.first() {
        Some(body) => request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone()),
        None => request,
    };
    let response = request.send().await?;
    let status = response.status();
    let text = response.text().await?;
    let pretty = serde_json::from_str::<serde_json::Value>(&text)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or(text);
    println!("{status}\n{pretty}");
    Ok(())
}
