//! Capability tokens, ed25519-signed.
//!
//! Each tier (control-panel, project) holds a `SigningKey` and mints
//! tokens for the tier below. Verifiers hold only the issuer's
//! `VerifyingKey`. Claims encode the subject (who), the scope (what
//! they may access), and an expiry (unix seconds).
//!
//! Wire form: `base64url(postcard(SignedToken))`.

pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use lutin_ids::{SessionId, Slug, WorkflowId, identifier};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, Verifier};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Maximum accepted token size in bytes (post base64-decode).
const MAX_TOKEN_BYTES: usize = 4096;

/// Token subject (who the token was minted for). Non-empty, ≤ 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Subject(String);

/// Errors from `Subject::parse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectError {
    Empty,
    TooLong,
}

impl fmt::Display for SubjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubjectError::Empty => write!(f, "subject must not be empty"),
            SubjectError::TooLong => write!(f, "subject exceeds 128 chars"),
        }
    }
}

impl std::error::Error for SubjectError {}

impl Subject {
    pub fn parse(s: impl Into<String>) -> Result<Self, SubjectError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SubjectError::Empty);
        }
        if s.len() > 128 {
            return Err(SubjectError::TooLong);
        }
        Ok(Subject(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Subject {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        Subject::parse(s).map_err(serde::de::Error::custom)
    }
}

/// Token time-to-live, expressed as a Duration so seconds vs ms unit
/// confusion is impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ttl(Duration);

impl Ttl {
    pub fn from_secs(s: u64) -> Self {
        Ttl(Duration::from_secs(s))
    }

    pub fn as_duration(&self) -> Duration {
        self.0
    }
}

/// Resource a token grants access to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scope {
    /// Full control-panel access.
    ControlPanel,
    /// Access to one project by slug.
    Project(Slug),
    /// Access to one workflow session.
    WorkflowSession {
        project: Slug,
        workflow: WorkflowId,
        session: SessionId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    pub subject: Subject,
    pub scope: Scope,
    /// Unix seconds.
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignedToken {
    claims_bytes: Vec<u8>,
    #[serde(with = "serde_big_array::BigArray")]
    signature: [u8; 64],
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("base64: {0}")]
    Base64(base64::DecodeError),
    #[error("bad signature")]
    BadSignature,
    #[error("expired")]
    Expired,
    #[error("clock unavailable")]
    Clock,
    #[error("rng unavailable")]
    Rng,
    #[error("token too large")]
    TokenTooLarge,
    #[error("bad public key")]
    BadPublicKey,
}

/// Parse a base64url-encoded ed25519 public key (no padding).
/// Companion to `pubkey_to_string` for the env-var / file handoff.
pub fn pubkey_from_str(s: &str) -> Result<VerifyingKey, AuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .map_err(|_| AuthError::BadPublicKey)?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| AuthError::BadPublicKey)?;
    VerifyingKey::from_bytes(&arr).map_err(|_| AuthError::BadPublicKey)
}

pub fn pubkey_to_string(key: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(key.to_bytes())
}

pub fn generate_keypair() -> Result<SigningKey, AuthError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| AuthError::Rng)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Mint a token signed by `key`. Internal: callers must go through
/// `mint_with_ttl` so TTL policy cannot be bypassed.
fn mint(key: &SigningKey, claims: Claims) -> Result<String, AuthError> {
    let claims_bytes = postcard::to_allocvec(&claims)?;
    let signature: Signature = key.sign(&claims_bytes);
    let token = SignedToken {
        claims_bytes,
        signature: signature.to_bytes(),
    };
    let bytes = postcard::to_allocvec(&token)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn mint_with_ttl(
    key: &SigningKey,
    subject: Subject,
    scope: Scope,
    ttl: Ttl,
) -> Result<String, AuthError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::Clock)?
        .as_secs();
    mint(
        key,
        Claims {
            subject,
            scope,
            expires_at: now + ttl.as_duration().as_secs(),
        },
    )
}

/// Verify token against the issuer's pubkey, check expiry, return claims.
pub fn verify(token: &str, issuer: &VerifyingKey) -> Result<Claims, AuthError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(AuthError::TokenTooLarge);
    }
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(AuthError::Base64)?;
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(AuthError::TokenTooLarge);
    }
    let signed: SignedToken = postcard::from_bytes(&bytes)?;
    let signature = Signature::from_bytes(&signed.signature);
    issuer
        .verify(&signed.claims_bytes, &signature)
        .map_err(|_| AuthError::BadSignature)?;
    let claims: Claims = postcard::from_bytes(&signed.claims_bytes)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::Clock)?
        .as_secs();
    if now >= claims.expires_at {
        return Err(AuthError::Expired);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_and_verify() {
        let key = generate_keypair().unwrap();
        let pub_key = key.verifying_key();
        let token = mint_with_ttl(
            &key,
            Subject::parse("client-1").unwrap(),
            Scope::ControlPanel,
            Ttl::from_secs(60),
        )
        .unwrap();
        let claims = verify(&token, &pub_key).unwrap();
        assert_eq!(claims.subject.as_str(), "client-1");
        assert_eq!(claims.scope, Scope::ControlPanel);
    }

    #[test]
    fn rejects_wrong_issuer() {
        let key_a = generate_keypair().unwrap();
        let key_b = generate_keypair().unwrap();
        let token = mint_with_ttl(
            &key_a,
            Subject::parse("x").unwrap(),
            Scope::ControlPanel,
            Ttl::from_secs(60),
        )
        .unwrap();
        assert!(matches!(
            verify(&token, &key_b.verifying_key()),
            Err(AuthError::BadSignature)
        ));
    }

    #[test]
    fn rejects_expired() {
        let key = generate_keypair().unwrap();
        let claims = Claims {
            subject: Subject::parse("x").unwrap(),
            scope: Scope::ControlPanel,
            expires_at: 1,
        };
        let token = mint(&key, claims).unwrap();
        assert!(matches!(
            verify(&token, &key.verifying_key()),
            Err(AuthError::Expired)
        ));
    }

    #[test]
    fn rejects_tampered() {
        let key = generate_keypair().unwrap();
        let token = mint_with_ttl(
            &key,
            Subject::parse("x").unwrap(),
            Scope::ControlPanel,
            Ttl::from_secs(60),
        )
        .unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let tampered = URL_SAFE_NO_PAD.encode(bytes);
        assert!(matches!(
            verify(&tampered, &key.verifying_key()),
            Err(AuthError::BadSignature)
        ));
    }

    #[test]
    fn pubkey_round_trip() {
        let key = generate_keypair().unwrap();
        let vk = key.verifying_key();
        let parsed = pubkey_from_str(&pubkey_to_string(&vk)).unwrap();
        assert_eq!(parsed.to_bytes(), vk.to_bytes());
    }

    #[test]
    fn project_scope_roundtrip() {
        let key = generate_keypair().unwrap();
        let token = mint_with_ttl(
            &key,
            Subject::parse("c").unwrap(),
            Scope::Project(Slug::parse("foo").unwrap()),
            Ttl::from_secs(60),
        )
        .unwrap();
        let claims = verify(&token, &key.verifying_key()).unwrap();
        assert_eq!(claims.scope, Scope::Project(Slug::parse("foo").unwrap()));
    }
}
