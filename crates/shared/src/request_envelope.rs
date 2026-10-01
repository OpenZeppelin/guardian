//! The envelope a Guardian-executable proposal stores its serialized `TransactionRequest` in,
//! written by the SDKs and read by the server.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::execution::ExecutionFailureCode;

/// The envelope format this server writes and reads.
pub const ENVELOPE_FORMAT_VERSION: u32 = 1;

/// The Miden protocol line requests are serialized for and executed on, as `MAJOR.MINOR`.
pub const PROTOCOL_LINE: &str = "0.17";

/// A serialized `TransactionRequest` stored with a Guardian-executable
/// proposal. `serializer_id` names the `miden-client` version that wrote the
/// bytes, because request serialization carries no version tag of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionRequestEnvelope {
    pub format_version: u32,
    pub protocol_line: String,
    pub serializer_id: String,
    pub checksum: String,
    pub bytes: String,
}

/// Why a stored envelope cannot be decoded by this server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRejection {
    MalformedBytes,
    ChecksumMismatch,
    UnsupportedFormat { format_version: u32 },
    ProtocolLineMismatch { declared: String },
    SerializerNotAdmitted { declared: String },
}

impl EnvelopeRejection {
    pub fn failure_code(&self) -> ExecutionFailureCode {
        match self {
            EnvelopeRejection::MalformedBytes
            | EnvelopeRejection::ChecksumMismatch
            | EnvelopeRejection::UnsupportedFormat { .. } => ExecutionFailureCode::RequestCodec,
            EnvelopeRejection::ProtocolLineMismatch { .. }
            | EnvelopeRejection::SerializerNotAdmitted { .. } => {
                ExecutionFailureCode::ProtocolMismatch
            }
        }
    }
}

impl std::fmt::Display for EnvelopeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeRejection::MalformedBytes => write!(f, "stored request bytes are not base64"),
            EnvelopeRejection::ChecksumMismatch => {
                write!(f, "stored request checksum does not match its bytes")
            }
            EnvelopeRejection::UnsupportedFormat { format_version } => {
                write!(f, "unsupported request envelope format {format_version}")
            }
            EnvelopeRejection::ProtocolLineMismatch { declared } => write!(
                f,
                "request serialized for Miden {declared}, this server executes {PROTOCOL_LINE}"
            ),
            EnvelopeRejection::SerializerNotAdmitted { declared } => {
                write!(
                    f,
                    "request serializer {declared} is not admitted by this server"
                )
            }
        }
    }
}

impl TransactionRequestEnvelope {
    /// Wrap freshly serialized request bytes.
    pub fn seal(bytes: &[u8], serializer_id: &str) -> Self {
        Self {
            format_version: ENVELOPE_FORMAT_VERSION,
            protocol_line: PROTOCOL_LINE.to_string(),
            serializer_id: serializer_id.to_string(),
            checksum: checksum_of(bytes),
            bytes: BASE64.encode(bytes),
        }
    }

    /// The raw request bytes, returned only once the checksum matches and
    /// the format, protocol line and serializer are ones the reader accepts.
    /// Nothing is deserialized before every check passes.
    pub fn verified_bytes(
        &self,
        admits_serializer: impl Fn(&str) -> bool,
    ) -> Result<Vec<u8>, EnvelopeRejection> {
        let bytes = self.decoded_bytes()?;
        if checksum_of(&bytes) != self.checksum {
            return Err(EnvelopeRejection::ChecksumMismatch);
        }
        if self.format_version != ENVELOPE_FORMAT_VERSION {
            return Err(EnvelopeRejection::UnsupportedFormat {
                format_version: self.format_version,
            });
        }
        if self.protocol_line != PROTOCOL_LINE {
            return Err(EnvelopeRejection::ProtocolLineMismatch {
                declared: self.protocol_line.clone(),
            });
        }
        if !admits_serializer(&self.serializer_id) {
            return Err(EnvelopeRejection::SerializerNotAdmitted {
                declared: self.serializer_id.clone(),
            });
        }
        Ok(bytes)
    }

    /// The length of the raw request once its body is intact and in a format this server reads,
    /// for size limits counted over decoded bytes rather than their base64 form. The protocol
    /// line and serializer are left to execution, which is where they must match.
    pub fn stored_len(&self) -> Result<usize, EnvelopeRejection> {
        let bytes = self.decoded_bytes()?;
        if checksum_of(&bytes) != self.checksum {
            return Err(EnvelopeRejection::ChecksumMismatch);
        }
        if self.format_version != ENVELOPE_FORMAT_VERSION {
            return Err(EnvelopeRejection::UnsupportedFormat {
                format_version: self.format_version,
            });
        }
        Ok(bytes.len())
    }

    fn decoded_bytes(&self) -> Result<Vec<u8>, EnvelopeRejection> {
        BASE64
            .decode(&self.bytes)
            .map_err(|_| EnvelopeRejection::MalformedBytes)
    }
}

fn checksum_of(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED_MIDEN_CLIENT_VERSION: &str = "0.17.0-rc.4";

    fn admits(serializer_id: &str) -> bool {
        serializer_id == PINNED_MIDEN_CLIENT_VERSION
    }

    #[test]
    fn a_sealed_envelope_verifies_back_to_its_bytes() {
        let envelope = TransactionRequestEnvelope::seal(b"request", PINNED_MIDEN_CLIENT_VERSION);
        assert_eq!(envelope.format_version, 1);
        assert_eq!(envelope.protocol_line, "0.17");
        assert_eq!(envelope.verified_bytes(admits).unwrap(), b"request");
        assert_eq!(envelope.stored_len().unwrap(), 7);
    }

    #[test]
    fn checksum_is_lowercase_prefixed_sha256_of_the_raw_bytes() {
        let envelope = TransactionRequestEnvelope::seal(b"abc", PINNED_MIDEN_CLIENT_VERSION);
        assert_eq!(
            envelope.checksum,
            "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_tampered_body_is_rejected_on_its_checksum() {
        let mut envelope =
            TransactionRequestEnvelope::seal(b"request", PINNED_MIDEN_CLIENT_VERSION);
        envelope.bytes = BASE64.encode(b"tampered");
        let rejection = envelope.verified_bytes(admits).unwrap_err();
        assert_eq!(rejection, EnvelopeRejection::ChecksumMismatch);
        assert_eq!(rejection.failure_code(), ExecutionFailureCode::RequestCodec);
    }

    #[test]
    fn non_base64_bytes_are_a_codec_failure() {
        let mut envelope =
            TransactionRequestEnvelope::seal(b"request", PINNED_MIDEN_CLIENT_VERSION);
        envelope.bytes = "not base64!".to_string();
        assert_eq!(
            envelope.verified_bytes(admits).unwrap_err(),
            EnvelopeRejection::MalformedBytes
        );
    }

    #[test]
    fn an_unsupported_format_is_a_codec_failure() {
        let mut envelope =
            TransactionRequestEnvelope::seal(b"request", PINNED_MIDEN_CLIENT_VERSION);
        envelope.format_version = 2;
        let rejection = envelope.verified_bytes(admits).unwrap_err();
        assert_eq!(
            rejection,
            EnvelopeRejection::UnsupportedFormat { format_version: 2 }
        );
        assert_eq!(rejection.failure_code(), ExecutionFailureCode::RequestCodec);
    }

    #[test]
    fn another_protocol_line_is_a_protocol_mismatch_compared_exactly() {
        for declared in ["0.16", "0.17.0", "v0.17", "0.18"] {
            let mut envelope =
                TransactionRequestEnvelope::seal(b"request", PINNED_MIDEN_CLIENT_VERSION);
            envelope.protocol_line = declared.to_string();
            let rejection = envelope.verified_bytes(admits).unwrap_err();
            assert_eq!(
                rejection,
                EnvelopeRejection::ProtocolLineMismatch {
                    declared: declared.to_string()
                }
            );
            assert_eq!(
                rejection.failure_code(),
                ExecutionFailureCode::ProtocolMismatch
            );
        }
    }

    #[test]
    fn an_unadmitted_serializer_on_the_same_line_is_a_protocol_mismatch() {
        let envelope = TransactionRequestEnvelope::seal(b"request", "0.17.0-rc.3");
        let rejection = envelope.verified_bytes(admits).unwrap_err();
        assert_eq!(
            rejection,
            EnvelopeRejection::SerializerNotAdmitted {
                declared: "0.17.0-rc.3".to_string()
            }
        );
        assert_eq!(
            rejection.failure_code(),
            ExecutionFailureCode::ProtocolMismatch
        );
    }

    #[test]
    fn the_shared_fixture_seals_and_is_judged_the_same_in_every_implementation() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/miden-multisig-client/request-envelope.json"
        ))
        .unwrap();
        let bytes = hex::decode(fixture["bytes_hex"].as_str().unwrap()).unwrap();
        let serializer = fixture["serializer_id"].as_str().unwrap();
        let admitted = fixture["admitted_serializer"].as_str().unwrap().to_string();
        let sealed = TransactionRequestEnvelope::seal(&bytes, serializer);
        assert_eq!(serde_json::to_value(&sealed).unwrap(), fixture["sealed"]);

        for case in fixture["cases"].as_array().unwrap() {
            let envelope: TransactionRequestEnvelope =
                serde_json::from_value(case["envelope"].clone()).unwrap();
            let outcome = match envelope.verified_bytes(|id| id == admitted) {
                Ok(verified) => {
                    assert_eq!(verified, bytes);
                    "accepted"
                }
                Err(EnvelopeRejection::ChecksumMismatch) => "checksum_mismatch",
                Err(EnvelopeRejection::UnsupportedFormat { .. }) => "unsupported_format",
                Err(EnvelopeRejection::ProtocolLineMismatch { .. }) => "protocol_line_mismatch",
                Err(EnvelopeRejection::SerializerNotAdmitted { .. }) => "serializer_not_admitted",
                Err(EnvelopeRejection::MalformedBytes) => "malformed_bytes",
            };
            assert_eq!(outcome, case["expected"], "{}", case["name"]);
        }
    }
}
