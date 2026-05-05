//! Claude Code billing-header fingerprint.
//!
//! Anthropic's subscription inference endpoint gates model access on a
//! fingerprint shipped as the first `system` block in the `/v1/messages`
//! body. The shape is reverse-engineered from the Claude Code CLI; if
//! Anthropic rotates `CCH_SALT` or the sampled positions, this breaks
//! silently and requests fall back to the public-API model set.
//!
//! The wire value looks like:
//!   x-anthropic-billing-header: cc_version=2.1.87.abc; cc_entrypoint=sdk-cli; cch=12345;
//!
//! - `cc_version` is `<CLAUDE_CODE_VERSION>.<3-char hash suffix>`
//! - `cc_entrypoint` is a free-form tag (we use `sdk-cli` to match Claude Code)
//! - `cch` is the first 5 hex chars of `sha256(first_user_message_text)`
//!
//! The 3-char version suffix is `sha256(SALT + sampled_chars + version)[:3]`
//! where `sampled_chars` is the first-user-message characters at positions
//! `[4, 7, 20]` (or `'0'` if out-of-bounds).

use sha2::{Digest, Sha256};

pub const CLAUDE_CODE_VERSION: &str = "2.1.87";
pub const CLAUDE_CODE_ENTRYPOINT: &str = "sdk-cli";
pub const CCH_SALT: &str = "59cf53e54c78";
pub const CCH_POSITIONS: [usize; 3] = [4, 7, 20];

fn hex_prefix(data: &[u8], n: usize) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(n);
    for b in digest.iter().take(n.div_ceil(2)) {
        out.push_str(&format!("{:02x}", b));
    }
    out.truncate(n);
    out
}

/// First 5 hex chars of sha256(message_text).
pub fn compute_cch(message_text: &str) -> String {
    hex_prefix(message_text.as_bytes(), 5)
}

/// 3-char sha256 tag over salt + sampled chars + version.
pub fn compute_version_suffix(message_text: &str, version: &str) -> String {
    let chars: String = CCH_POSITIONS
        .iter()
        .map(|&i| message_text.chars().nth(i).unwrap_or('0'))
        .collect();
    let input = format!("{CCH_SALT}{chars}{version}");
    hex_prefix(input.as_bytes(), 3)
}

/// Build the `x-anthropic-billing-header: ...` string that ships as the
/// first system block on OAuth requests.
pub fn build_billing_header(first_user_text: &str) -> String {
    let suffix = compute_version_suffix(first_user_text, CLAUDE_CODE_VERSION);
    let cch = compute_cch(first_user_text);
    format!(
        "x-anthropic-billing-header: cc_version={CLAUDE_CODE_VERSION}.{suffix}; \
         cc_entrypoint={CLAUDE_CODE_ENTRYPOINT}; cch={cch};"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cch_is_5_hex_chars() {
        let c = compute_cch("hello world");
        assert_eq!(c.len(), 5);
        assert!(c.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn suffix_is_3_hex_chars() {
        let s = compute_version_suffix("0123456789abcdefghij0123", "2.1.87");
        assert_eq!(s.len(), 3);
        assert!(s.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn short_text_pads_with_zero() {
        // "ab" — all 3 sample positions are OOB, so all sampled chars = '0'.
        let a = compute_version_suffix("ab", "2.1.87");
        let b = compute_version_suffix("cd", "2.1.87");
        assert_eq!(a, b);
    }

    #[test]
    fn header_has_expected_shape() {
        let h = build_billing_header("what's the weather");
        assert!(h.starts_with("x-anthropic-billing-header: "));
        assert!(h.contains("cc_version=2.1.87."));
        assert!(h.contains("cc_entrypoint=sdk-cli"));
        assert!(h.contains("cch="));
    }
}
