//! Wire envelope shared by control-panel, project, and workflow tiers.
//!
//! Each tier defines its own payload enum and serialises it into the
//! `Payload` / `Broadcast` byte slots. This crate owns nothing tier-specific.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Frame {
    Hello {
        protocol_version: u32,
        token: String,
    },
    HelloAck(HandshakeResult),
    /// Request/response payload, opaque to this crate.
    Payload {
        request_id: u64,
        body: Vec<u8>,
    },
    /// Server-pushed event, no correlation id.
    Broadcast {
        body: Vec<u8>,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Close {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HandshakeResult {
    Accepted,
    Rejected { reason: String },
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
}

pub fn encode(frame: &Frame) -> Result<Vec<u8>, CodecError> {
    Ok(postcard::to_allocvec(frame)?)
}

pub fn decode(bytes: &[u8]) -> Result<Frame, CodecError> {
    Ok(postcard::from_bytes(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hello() {
        let f = Frame::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "abc".into(),
        };
        assert_eq!(decode(&encode(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn roundtrip_payload() {
        let f = Frame::Payload {
            request_id: 42,
            body: vec![1, 2, 3, 4],
        };
        assert_eq!(decode(&encode(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn roundtrip_broadcast() {
        let f = Frame::Broadcast {
            body: b"event".to_vec(),
        };
        assert_eq!(decode(&encode(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn roundtrip_hello_ack_accepted() {
        let f = Frame::HelloAck(HandshakeResult::Accepted);
        assert_eq!(decode(&encode(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn roundtrip_hello_ack_rejected() {
        let f = Frame::HelloAck(HandshakeResult::Rejected {
            reason: "nope".into(),
        });
        assert_eq!(decode(&encode(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn decode_garbage_errors() {
        assert!(decode(&[0xff; 4]).is_err());
    }

    /// Golden-bytes regression: pins `Frame::Payload { request_id: 1, body: vec![0xAA] }`
    /// to its exact postcard encoding. Any drift in wire format will trip this.
    #[test]
    fn golden_payload_bytes() {
        let f = Frame::Payload {
            request_id: 1,
            body: vec![0xAA],
        };
        let bytes = encode(&f).unwrap();
        assert_eq!(bytes, vec![0x02, 0x01, 0x01, 0xAA]);
        assert_eq!(decode(&bytes).unwrap(), f);
    }
}
