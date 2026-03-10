use base64::Engine;

/// Decode JWT payload claims without signature verification.
/// Used only to check `exp` (expiry) timestamps for token refresh decisions.
pub fn decode_claims(token: &str) -> Option<serde_json::Value> {
    let mut iter = token.splitn(4, '.');
    let _header = iter.next()?;
    let payload = iter.next()?;
    let _sig = iter.next()?;
    if iter.next().is_some() {
        return None; // more than 3 parts
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| {
            // Try with padding
            let padded = match payload.len() % 4 {
                2 => format!("{payload}=="),
                3 => format!("{payload}="),
                _ => payload.to_owned(),
            };
            base64::engine::general_purpose::URL_SAFE.decode(&padded)
        })
        .ok()?;

    serde_json::from_slice(&bytes).ok()
}

/// Check if a JWT access token is expiring within `skew_seconds`.
/// Returns `false` if the token is malformed or has no `exp` claim
/// (conservative: don't force unnecessary refreshes).
pub fn is_expiring(token: &str, skew_seconds: i64) -> bool {
    let claims = match decode_claims(token) {
        Some(c) => c,
        None => return false,
    };

    let exp = match claims.get("exp").and_then(|v| v.as_f64()) {
        Some(e) => e,
        None => return false,
    };

    let now = chrono::Utc::now().timestamp() as f64;
    exp <= (now + skew_seconds as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn make_jwt(claims: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_string(claims).unwrap());
        let signature =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("fake-signature");
        format!("{header}.{payload}.{signature}")
    }

    #[test]
    fn decode_claims_extracts_payload() {
        let claims = serde_json::json!({"sub": "user123", "exp": 9999999999_u64});
        let token = make_jwt(&claims);
        let decoded = decode_claims(&token).unwrap();
        assert_eq!(decoded["sub"], "user123");
        assert_eq!(decoded["exp"], 9999999999_u64);
    }

    #[test]
    fn decode_claims_returns_none_for_non_jwt() {
        assert!(decode_claims("not-a-jwt").is_none());
        assert!(decode_claims("").is_none());
        assert!(decode_claims("one.two").is_none());
    }

    #[test]
    fn decode_claims_handles_padded_base64() {
        let claims = serde_json::json!({"x": "a"});
        let token = make_jwt(&claims);
        let decoded = decode_claims(&token).unwrap();
        assert_eq!(decoded["x"], "a");
    }

    #[test]
    fn is_expiring_returns_false_for_far_future_token() {
        let claims = serde_json::json!({"exp": 9999999999_u64});
        let token = make_jwt(&claims);
        assert!(!is_expiring(&token, 120));
    }

    #[test]
    fn is_expiring_returns_true_for_expired_token() {
        let claims = serde_json::json!({"exp": 1000000000_u64});
        let token = make_jwt(&claims);
        assert!(is_expiring(&token, 120));
    }

    #[test]
    fn is_expiring_returns_false_for_missing_exp() {
        let claims = serde_json::json!({"sub": "user"});
        let token = make_jwt(&claims);
        assert!(!is_expiring(&token, 120));
    }

    #[test]
    fn is_expiring_returns_false_for_malformed_token() {
        assert!(!is_expiring("garbage", 120));
    }
}
