//! Version 2 of session-signed per-account requests.
//!
//! A session key signs a fixed 62-byte envelope that binds the transport and
//! the operation in addition to the account, timestamp and payload already
//! covered by [`AuthRequestMessage`], so a request signed for one route can
//! never be accepted on another, even when their payloads serialize to the
//! same bytes:
//!
//! ```text
//! "GUARDIAN_SESSION_REQUEST" 0x00 || u16be(2) || u8(transport)
//!     || u16be(operation) || AuthRequestMessage word (32 bytes)
//! ```
//!
//! P-256 signs these bytes with SHA-256, exactly as WebCrypto's
//! `sign({ name: "ECDSA", hash: "SHA-256" }, key, envelope)` does.

use miden_protocol::Word;
use sha2::{Digest, Sha256};

use crate::auth_request_message::AuthRequestMessage;

/// Domain prefix, with its trailing zero byte.
pub const SESSION_REQUEST_DOMAIN: &[u8; 25] = b"GUARDIAN_SESSION_REQUEST\0";

/// The envelope version this module encodes.
pub const SESSION_REQUEST_VERSION: u16 = 2;

/// Value of the `x-auth-format` header or metadata entry for this version.
pub const SESSION_REQUEST_AUTH_FORMAT: &str = "session-v2";

/// Length of an encoded envelope.
pub const SESSION_REQUEST_ENVELOPE_LEN: usize = 25 + 2 + 1 + 2 + 32;

/// How a request reached the server. Equivalent HTTP and gRPC requests carry
/// different signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionTransport {
    Http = 1,
    Grpc = 2,
}

impl SessionTransport {
    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Http),
            2 => Some(Self::Grpc),
            _ => None,
        }
    }
}

/// Which replay rule a session operation is checked under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReplayPolicy {
    Read,
    Write,
}

/// Every per-account operation a session may sign. Ids are permanent: never
/// renumber or reuse one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionOperation {
    GetState = 1,
    GetCanonicalNonce = 2,
    GetDelta = 3,
    GetDeltaSince = 4,
    GetDeltaHistory = 5,
    GetDeltaProposals = 6,
    GetDeltaProposal = 7,
    PushDelta = 8,
    PushDeltaProposal = 9,
    SignDeltaProposal = 10,
    ExecuteDeltaProposal = 11,
    GetDeltaProposalExecution = 12,
    GetCurrentExecution = 13,
    GetStateTail = 14,
}

impl SessionOperation {
    pub const ALL: [Self; 14] = [
        Self::GetState,
        Self::GetCanonicalNonce,
        Self::GetDelta,
        Self::GetDeltaSince,
        Self::GetDeltaHistory,
        Self::GetDeltaProposals,
        Self::GetDeltaProposal,
        Self::PushDelta,
        Self::PushDeltaProposal,
        Self::SignDeltaProposal,
        Self::ExecuteDeltaProposal,
        Self::GetDeltaProposalExecution,
        Self::GetCurrentExecution,
        Self::GetStateTail,
    ];

    pub fn id(self) -> u16 {
        self as u16
    }

    pub fn from_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|operation| operation.id() == id)
    }

    pub fn replay_policy(self) -> ReplayPolicy {
        match self {
            Self::GetState
            | Self::GetCanonicalNonce
            | Self::GetDelta
            | Self::GetDeltaSince
            | Self::GetDeltaHistory
            | Self::GetDeltaProposals
            | Self::GetDeltaProposal
            | Self::GetDeltaProposalExecution
            | Self::GetCurrentExecution
            | Self::GetStateTail => ReplayPolicy::Read,
            Self::PushDelta
            | Self::PushDeltaProposal
            | Self::SignDeltaProposal
            | Self::ExecuteDeltaProposal => ReplayPolicy::Write,
        }
    }

    /// The gRPC method name on `guardian.Guardian`, also used as a stable
    /// name in logs and test vectors.
    pub fn name(self) -> &'static str {
        match self {
            Self::GetState => "GetState",
            Self::GetCanonicalNonce => "GetCanonicalNonce",
            Self::GetDelta => "GetDelta",
            Self::GetDeltaSince => "GetDeltaSince",
            Self::GetDeltaHistory => "GetDeltaHistory",
            Self::GetDeltaProposals => "GetDeltaProposals",
            Self::GetDeltaProposal => "GetDeltaProposal",
            Self::PushDelta => "PushDelta",
            Self::PushDeltaProposal => "PushDeltaProposal",
            Self::SignDeltaProposal => "SignDeltaProposal",
            Self::ExecuteDeltaProposal => "ExecuteDeltaProposal",
            Self::GetDeltaProposalExecution => "GetDeltaProposalExecution",
            Self::GetCurrentExecution => "GetCurrentExecution",
            Self::GetStateTail => "GetStateTail",
        }
    }
}

/// The bytes a session key signs for one per-account request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRequestEnvelope {
    transport: SessionTransport,
    operation: SessionOperation,
    request_word: Word,
}

impl SessionRequestEnvelope {
    pub fn new(
        transport: SessionTransport,
        operation: SessionOperation,
        message: &AuthRequestMessage,
    ) -> Self {
        Self::from_request_word(transport, operation, message.to_word())
    }

    pub fn from_request_word(
        transport: SessionTransport,
        operation: SessionOperation,
        request_word: Word,
    ) -> Self {
        Self {
            transport,
            operation,
            request_word,
        }
    }

    pub fn transport(&self) -> SessionTransport {
        self.transport
    }

    pub fn operation(&self) -> SessionOperation {
        self.operation
    }

    pub fn to_bytes(&self) -> [u8; SESSION_REQUEST_ENVELOPE_LEN] {
        let mut bytes = [0u8; SESSION_REQUEST_ENVELOPE_LEN];
        bytes[..25].copy_from_slice(SESSION_REQUEST_DOMAIN);
        bytes[25..27].copy_from_slice(&SESSION_REQUEST_VERSION.to_be_bytes());
        bytes[27] = self.transport.id();
        bytes[28..30].copy_from_slice(&self.operation.id().to_be_bytes());
        bytes[30..].copy_from_slice(&self.request_word.as_bytes());
        bytes
    }

    /// SHA-256 of the envelope: the digest P-256 signs.
    pub fn signed_digest(&self) -> [u8; 32] {
        Sha256::digest(self.to_bytes()).into()
    }

    /// Key of the server's record of accepted reads: SHA-256 of the session
    /// public key followed by the envelope. It never depends on signature
    /// bytes, so the two valid encodings of one signature are one request.
    pub fn record_key(&self, session_public_key: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(session_public_key);
        hasher.update(self.to_bytes());
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(
        transport: SessionTransport,
        operation: SessionOperation,
    ) -> SessionRequestEnvelope {
        SessionRequestEnvelope::from_request_word(transport, operation, Word::from([1u32, 2, 3, 4]))
    }

    #[test]
    fn layout_is_domain_version_transport_operation_word() {
        let bytes =
            envelope(SessionTransport::Grpc, SessionOperation::SignDeltaProposal).to_bytes();
        assert_eq!(bytes.len(), 62);
        assert_eq!(&bytes[..25], b"GUARDIAN_SESSION_REQUEST\0");
        assert_eq!(&bytes[25..27], &[0, 2]);
        assert_eq!(bytes[27], 2);
        assert_eq!(&bytes[28..30], &[0, 10]);
        assert_eq!(&bytes[30..], &Word::from([1u32, 2, 3, 4]).as_bytes());
    }

    #[test]
    fn operation_ids_round_trip_and_unknown_ids_are_refused() {
        for operation in SessionOperation::ALL {
            assert_eq!(SessionOperation::from_id(operation.id()), Some(operation));
        }
        assert_eq!(SessionOperation::from_id(0), None);
        assert_eq!(SessionOperation::from_id(15), None);
        assert_eq!(SessionTransport::from_id(3), None);
    }

    #[test]
    fn every_field_changes_the_signed_digest() {
        let base = envelope(SessionTransport::Http, SessionOperation::GetState);
        let others = [
            envelope(SessionTransport::Grpc, SessionOperation::GetState),
            envelope(SessionTransport::Http, SessionOperation::GetCanonicalNonce),
            SessionRequestEnvelope::from_request_word(
                SessionTransport::Http,
                SessionOperation::GetState,
                Word::from([1u32, 2, 3, 5]),
            ),
        ];
        for other in others {
            assert_ne!(base.signed_digest(), other.signed_digest());
        }
    }

    #[test]
    fn record_key_binds_the_session_key() {
        let envelope = envelope(SessionTransport::Http, SessionOperation::GetState);
        assert_ne!(envelope.record_key(&[2; 33]), envelope.record_key(&[3; 33]));
    }

    #[test]
    fn reads_and_writes_follow_the_registry() {
        let writes: Vec<_> = SessionOperation::ALL
            .into_iter()
            .filter(|operation| operation.replay_policy() == ReplayPolicy::Write)
            .collect();
        assert_eq!(
            writes,
            [
                SessionOperation::PushDelta,
                SessionOperation::PushDeltaProposal,
                SessionOperation::SignDeltaProposal,
                SessionOperation::ExecuteDeltaProposal,
            ]
        );
    }
}
