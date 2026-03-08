//! Webhook signature verification for platform integrations.
//!
//! Each platform uses its own signing mechanism:
//! - **Slack**: HMAC-SHA256 with a signing secret
//! - **Discord**: Ed25519 signature verification with a public key

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verify a Slack webhook request signature.
///
/// Slack sends three headers:
/// - `X-Slack-Request-Timestamp` — Unix timestamp of the request
/// - `X-Slack-Signature` — `v0=<hex HMAC-SHA256 signature>`
///
/// The signature is computed over `v0:{timestamp}:{body}` using the
/// signing secret as key.
///
/// Returns `true` if the signature is valid.
pub fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    // Guard against replay attacks (reject requests older than 5 minutes)
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if (now - ts).unsigned_abs() > 300 {
            return false;
        }
    }

    // Build the base string: v0:{timestamp}:{body}
    let body_str = std::str::from_utf8(body).unwrap_or("");
    let base_string = format!("v0:{timestamp}:{body_str}");

    // Compute the HMAC
    let mut mac = match HmacSha256::new_from_slice(signing_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(base_string.as_bytes());

    // The expected signature format is "v0=<hex>"
    let expected = match signature.strip_prefix("v0=") {
        Some(hex_sig) => hex_sig,
        None => return false,
    };

    let computed = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(computed.as_bytes(), expected.as_bytes())
}

/// Verify a Discord interaction signature using Ed25519.
///
/// Discord sends two headers:
/// - `X-Signature-Ed25519` — hex-encoded Ed25519 signature
/// - `X-Signature-Timestamp` — timestamp string
///
/// The signature is computed over `{timestamp}{body}` using the
/// application's public key.
pub fn verify_discord_signature(
    public_key_hex: &str,
    timestamp: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};

    // Decode the public key from hex
    let pubkey_bytes = match hex::decode(public_key_hex) {
        Ok(bytes) if bytes.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            arr
        }
        _ => return false,
    };

    let verifying_key = match VerifyingKey::from_bytes(&pubkey_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };

    // Decode the signature from hex
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(bytes) if bytes.len() == 64 => {
            let mut arr = [0u8; 64];
            arr.copy_from_slice(&bytes);
            arr
        }
        _ => return false,
    };

    let signature = Signature::from_bytes(&sig_bytes);

    // The message is timestamp + body
    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);

    use ed25519_dalek::Verifier;
    verifying_key.verify(&message, &signature).is_ok()
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_signature_valid() {
        let secret = "8f742231b10e8888abcd99yez67800";
        let timestamp = "1531420618";
        let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&api_app_id=A19GAU123";

        // Compute the expected signature
        let base_string = format!("v0:{timestamp}:{}", std::str::from_utf8(body).unwrap());
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base_string.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        // Use a fixed "now" time that's within 5 minutes of the timestamp
        // Since verify_slack_signature checks against real time, we test the
        // HMAC logic by creating a signature with a recent timestamp.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let recent_ts = now.to_string();

        let base_recent = format!("v0:{recent_ts}:{}", std::str::from_utf8(body).unwrap());
        let mut mac2 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac2.update(base_recent.as_bytes());
        let sig_recent = format!("v0={}", hex::encode(mac2.finalize().into_bytes()));

        assert!(verify_slack_signature(secret, &recent_ts, body, &sig_recent));
    }

    #[test]
    fn slack_signature_wrong_secret() {
        let body = b"test body";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = now.to_string();

        let base = format!("v0:{ts}:{}", std::str::from_utf8(body).unwrap());
        let mut mac = HmacSha256::new_from_slice(b"correct-secret").unwrap();
        mac.update(base.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        assert!(!verify_slack_signature("wrong-secret", &ts, body, &sig));
    }

    #[test]
    fn slack_rejects_old_timestamp() {
        let body = b"test";
        // Timestamp from 10 minutes ago
        let old_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600)
            .to_string();

        let base = format!("v0:{old_ts}:test");
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(base.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        assert!(!verify_slack_signature("secret", &old_ts, body, &sig));
    }

    #[test]
    fn slack_rejects_missing_v0_prefix() {
        assert!(!verify_slack_signature("secret", "123", b"body", "badhex"));
    }

    #[test]
    fn discord_signature_rejects_invalid_key() {
        assert!(!verify_discord_signature("not-hex", "ts", b"body", "sig"));
    }

    #[test]
    fn discord_signature_rejects_wrong_length_key() {
        let short_hex = hex::encode([0u8; 16]); // 16 bytes, needs 32
        assert!(!verify_discord_signature(&short_hex, "ts", b"body", "sig"));
    }

    #[test]
    fn discord_signature_rejects_invalid_signature_hex() {
        let key_hex = hex::encode([1u8; 32]);
        assert!(!verify_discord_signature(&key_hex, "ts", b"body", "not-hex"));
    }

    #[test]
    fn discord_signature_rejects_wrong_signature() {
        use ed25519_dalek::SigningKey;

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let pub_key_hex = hex::encode(verifying_key.as_bytes());

        // Sign a different message
        use ed25519_dalek::Signer;
        let sig = signing_key.sign(b"different message");
        let sig_hex = hex::encode(sig.to_bytes());

        assert!(!verify_discord_signature(
            &pub_key_hex,
            "timestamp",
            b"actual body",
            &sig_hex
        ));
    }

    #[test]
    fn discord_signature_valid() {
        use ed25519_dalek::SigningKey;

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let pub_key_hex = hex::encode(verifying_key.as_bytes());

        let timestamp = "1234567890";
        let body = b"{\"type\":1}";

        // Sign timestamp + body
        let mut message = Vec::new();
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(body);

        use ed25519_dalek::Signer;
        let sig = signing_key.sign(&message);
        let sig_hex = hex::encode(sig.to_bytes());

        assert!(verify_discord_signature(
            &pub_key_hex,
            timestamp,
            body,
            &sig_hex
        ));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
    }
}
