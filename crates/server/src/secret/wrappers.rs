use std::fmt;

use secrecy::{ExposeSecret, SecretBox};
use subtle::ConstantTimeEq;
use url::Url;

pub(crate) struct FixedKey<const N: usize> {
    inner: SecretBox<[u8; N]>,
}

impl<const N: usize> FixedKey<N> {
    pub(crate) fn new(bytes: [u8; N]) -> Self {
        Self {
            inner: SecretBox::new(Box::new(bytes)),
        }
    }

    pub(crate) fn expose_secret(&self) -> &[u8; N] {
        self.inner.expose_secret()
    }
}

impl<const N: usize> Clone for FixedKey<N> {
    fn clone(&self) -> Self {
        Self::new(*self.expose_secret())
    }
}

impl<const N: usize> fmt::Debug for FixedKey<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FixedKey<{N}>(<redacted>)")
    }
}

impl<const N: usize> PartialEq for FixedKey<N> {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret().ct_eq(other.expose_secret()).into()
    }
}

impl<const N: usize> Eq for FixedKey<N> {}

pub(crate) struct SecretBytes {
    inner: SecretBox<Vec<u8>>,
}

impl SecretBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self {
            inner: SecretBox::new(Box::new(bytes)),
        }
    }

    pub(crate) fn expose_secret(&self) -> &[u8] {
        self.inner.expose_secret().as_slice()
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.expose_secret().len()
    }
}

impl Clone for SecretBytes {
    fn clone(&self) -> Self {
        Self::new(self.expose_secret().to_vec())
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes(len={})", self.len())
    }
}

impl PartialEq for SecretBytes {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret().ct_eq(other.expose_secret()).into()
    }
}

impl Eq for SecretBytes {}

pub(crate) struct SecretString {
    inner: SecretBox<String>,
}

impl SecretString {
    pub(crate) fn new(s: String) -> Self {
        Self {
            inner: SecretBox::new(Box::new(s)),
        }
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.inner.expose_secret().as_str()
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.expose_secret().len()
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self::new(self.expose_secret().to_owned())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString(len={})", self.len())
    }
}

impl PartialEq for SecretString {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret()
            .as_bytes()
            .ct_eq(other.expose_secret().as_bytes())
            .into()
    }
}

impl Eq for SecretString {}

pub(crate) struct CredentialUrl {
    inner: SecretBox<String>,
}

impl CredentialUrl {
    pub(crate) fn new(url: String) -> Self {
        Self {
            inner: SecretBox::new(Box::new(url)),
        }
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.inner.expose_secret().as_str()
    }

    /// Returns `scheme://host[:port]` with userinfo, path, and query stripped.
    /// Safe to log. Returns `<invalid-url>` if parsing fails.
    pub(crate) fn scheme_and_host(&self) -> String {
        scheme_and_host(self.expose_secret())
    }
}

impl Clone for CredentialUrl {
    fn clone(&self) -> Self {
        Self::new(self.expose_secret().to_owned())
    }
}

impl fmt::Debug for CredentialUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CredentialUrl({})", self.scheme_and_host())
    }
}

impl PartialEq for CredentialUrl {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret()
            .as_bytes()
            .ct_eq(other.expose_secret().as_bytes())
            .into()
    }
}

impl Eq for CredentialUrl {}

fn scheme_and_host(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return "<invalid-url>".to_owned();
    };
    let Some(host) = url.host_str() else {
        return "<invalid-url>".to_owned();
    };
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}
