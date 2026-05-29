use std::fmt::{self, Debug, Display};

use serde::{Deserialize, Serialize};
use static_assertions::{assert_impl_all, assert_not_impl_any};

use super::{CredentialUrl, FixedKey, SecretBytes, SecretString, ct_eq};

assert_not_impl_any!(FixedKey<32>: Display, Serialize, Deserialize<'static>);
assert_not_impl_any!(SecretBytes: Display, Serialize, Deserialize<'static>);
assert_not_impl_any!(SecretString: Display, Serialize, Deserialize<'static>);
assert_not_impl_any!(CredentialUrl: Display, Serialize, Deserialize<'static>);

// Public response DTOs must keep their Serialize impls. Combined with the
// non-impl assertions above, adding a wrapper field to any of these DTOs
// becomes a compile error: #[derive(Serialize)] on the DTO would fail
// because the wrapper does not implement Serialize. Sample chosen to span
// the modules adjacent to wrapper-bearing state — dashboard, EVM, and the
// storage-info layer.
assert_impl_all!(crate::services::DashboardAccountSummary: Serialize);
assert_impl_all!(crate::services::DashboardDeltaEntry: Serialize);
assert_impl_all!(crate::services::DashboardGlobalDeltaEntry: Serialize);
assert_impl_all!(crate::services::DashboardProposalEntry: Serialize);
assert_impl_all!(crate::services::DashboardGlobalProposalEntry: Serialize);
assert_impl_all!(crate::services::DashboardInfoResponse: Serialize);
#[cfg(feature = "evm")]
assert_impl_all!(crate::api::evm::VerifySessionResponse: Serialize);
#[cfg(feature = "evm")]
assert_impl_all!(crate::api::evm::ChallengeResponse: Serialize);

fn debug_str<T: Debug>(value: &T) -> String {
    let mut out = String::new();
    fmt::write(&mut out, format_args!("{value:?}")).unwrap();
    out
}

#[test]
fn fixed_key_debug_redacts() {
    let key = FixedKey::<32>::new([0xAB; 32]);
    let rendered = debug_str(&key);
    assert!(rendered.contains("FixedKey<32>"));
    assert!(rendered.contains("<redacted>"));
    assert!(!rendered.contains("ab"));
    assert!(!rendered.contains("AB"));
}

#[test]
fn secret_bytes_debug_redacts() {
    let bytes = SecretBytes::new(b"correct horse battery staple".to_vec());
    let rendered = debug_str(&bytes);
    assert!(rendered.starts_with("SecretBytes(len="));
    assert!(!rendered.contains("horse"));
}

#[test]
fn secret_string_debug_redacts() {
    let s = SecretString::new("super-secret-token".to_owned());
    let rendered = debug_str(&s);
    assert!(rendered.starts_with("SecretString(len="));
    assert!(!rendered.contains("super-secret-token"));
}

#[test]
fn credential_url_debug_shows_only_scheme_and_host() {
    let url = CredentialUrl::new("postgres://alice:hunter2@db.example.com:5432/app".to_owned());
    let rendered = debug_str(&url);
    assert!(rendered.contains("postgres://db.example.com:5432"));
    assert!(!rendered.contains("hunter2"));
    assert!(!rendered.contains("alice"));
    assert!(!rendered.contains("/app"));
}

#[test]
fn credential_url_scheme_and_host_strips_userinfo_and_query() {
    let url = CredentialUrl::new("postgres://alice:hunter2@db.example.com:5432/app".to_owned());
    assert_eq!(url.scheme_and_host(), "postgres://db.example.com:5432");

    let api = CredentialUrl::new("https://api.example.com/v1/?key=abc".to_owned());
    assert_eq!(api.scheme_and_host(), "https://api.example.com");

    let plain = CredentialUrl::new("https://example.com".to_owned());
    assert_eq!(plain.scheme_and_host(), "https://example.com");

    let bad = CredentialUrl::new("not-a-url".to_owned());
    assert_eq!(bad.scheme_and_host(), "<invalid-url>");

    let at_in_path = CredentialUrl::new("https://api.example.com/users/me@host".to_owned());
    assert_eq!(at_in_path.scheme_and_host(), "https://api.example.com");

    let at_in_query = CredentialUrl::new("https://api.example.com/?to=me@host".to_owned());
    assert_eq!(at_in_query.scheme_and_host(), "https://api.example.com");

    // Userinfo without an '@' delimiter (malformed authority): the previous
    // string-splitting heuristic would treat `alice:secret` as `host:port`
    // and emit the secret verbatim. Confirm it now renders as <invalid-url>
    // (port is non-numeric, so url::Url rejects the input).
    let no_at = CredentialUrl::new("postgres://alice:secret/db".to_owned());
    let rendered = no_at.scheme_and_host();
    assert!(
        !rendered.contains("secret"),
        "scheme_and_host leaked password portion: {rendered}"
    );
}

#[test]
fn clone_produces_independent_buffer() {
    let original = SecretBytes::new(vec![1, 2, 3, 4]);
    let cloned = original.clone();
    drop(original);
    assert_eq!(cloned.expose_secret(), &[1, 2, 3, 4]);

    let original = SecretString::new("token".to_owned());
    let cloned = original.clone();
    drop(original);
    assert_eq!(cloned.expose_secret(), "token");

    let original = FixedKey::<4>::new([9, 8, 7, 6]);
    let cloned = original.clone();
    drop(original);
    assert_eq!(cloned.expose_secret(), &[9, 8, 7, 6]);
}

#[test]
fn equality_is_consistent_with_contents() {
    let a = FixedKey::<4>::new([1, 2, 3, 4]);
    let b = FixedKey::<4>::new([1, 2, 3, 4]);
    let c = FixedKey::<4>::new([1, 2, 3, 5]);
    assert_eq!(a, b);
    assert_ne!(a, c);

    let s1 = SecretString::new("alpha".to_owned());
    let s2 = SecretString::new("alpha".to_owned());
    let s3 = SecretString::new("beta".to_owned());
    assert_eq!(s1, s2);
    assert_ne!(s1, s3);

    let u1 = CredentialUrl::new("https://a.example.com".to_owned());
    let u2 = CredentialUrl::new("https://a.example.com".to_owned());
    let u3 = CredentialUrl::new("https://b.example.com".to_owned());
    assert_eq!(u1, u2);
    assert_ne!(u1, u3);
}

#[test]
fn ct_eq_distinguishes_equal_and_differing_inputs() {
    assert!(ct_eq(b"", b""));
    assert!(ct_eq(b"abcdef", b"abcdef"));
    assert!(!ct_eq(b"abcdef", b"abcdeg"));
    assert!(!ct_eq(b"xbcdef", b"abcdef"));
    assert!(!ct_eq(b"abcdef", b"abcdefg"));
    assert!(!ct_eq(b"", b"a"));
}

#[test]
fn secret_string_len_returns_byte_length() {
    let s = SecretString::new("héllo".to_owned());
    assert_eq!(s.len(), "héllo".len());
}

#[test]
fn secret_bytes_len_returns_inner_len() {
    let bytes = SecretBytes::new(vec![0; 12]);
    assert_eq!(bytes.len(), 12);
}
