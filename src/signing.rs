//! Request signing for a service that hands fill jobs to this library over
//! HTTP. The production worker signed every control-plane call this way, byte
//! for byte the same as its TypeScript predecessor, so the server could not
//! tell the two apart: HMAC-SHA256 over
//!
//!   METHOD \n pathname \n ISO-8601 timestamp (ms, Z) \n nonce \n sha256(body)
//!
//! sent as x-worker-{key-id,nonce,timestamp,signature}. The verifier side
//! (timestamp window, nonce replay cache, constant-time compare) lives on the
//! server; [`verify`] here is the constant-time compare.

use chrono::{DateTime, SecondsFormat, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

pub struct SignedHeaders {
    pub key_id: String,
    pub nonce: String,
    pub timestamp: String,
    pub signature: String,
}

impl SignedHeaders {
    pub fn pairs(&self) -> [(&'static str, &str); 4] {
        [
            ("x-worker-key-id", &self.key_id),
            ("x-worker-nonce", &self.nonce),
            ("x-worker-signature", &self.signature),
            ("x-worker-timestamp", &self.timestamp),
        ]
    }
}

pub fn sign(key_id: &str, secret: &str, method: &str, pathname: &str, body: &str) -> SignedHeaders {
    sign_at(
        key_id,
        secret,
        method,
        pathname,
        body,
        Utc::now(),
        &uuid::Uuid::new_v4().to_string(),
    )
}

fn mac_for(secret: &str, method: &str, pathname: &str, timestamp: &str, nonce: &str, body: &str) -> Hmac<Sha256> {
    let body_hash = hex::encode(Sha256::digest(body.as_bytes()));
    let canonical = [method.to_uppercase().as_str(), pathname, timestamp, nonce, &body_hash].join("\n");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(canonical.as_bytes());
    mac
}

pub fn sign_at(
    key_id: &str,
    secret: &str,
    method: &str,
    pathname: &str,
    body: &str,
    now: DateTime<Utc>,
    nonce: &str,
) -> SignedHeaders {
    let timestamp = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mac = mac_for(secret, method, pathname, &timestamp, nonce, body);
    SignedHeaders {
        key_id: key_id.to_string(),
        nonce: nonce.to_string(),
        timestamp,
        signature: hex::encode(mac.finalize().into_bytes()),
    }
}

/// Checks a received signature in constant time. Freshness of `timestamp`
/// and uniqueness of `nonce` are the caller's to enforce.
pub fn verify(
    secret: &str,
    method: &str,
    pathname: &str,
    body: &str,
    timestamp: &str,
    nonce: &str,
    signature_hex: &str,
) -> bool {
    let Ok(signature) = hex::decode(signature_hex) else {
        return false;
    };
    mac_for(secret, method, pathname, timestamp, nonce, body)
        .verify_slice(&signature)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "FAKE-SECRET-FOR-TESTS";
    const FIXED_KEY: &str = "FAKE-KEY-FOR-TESTS-ONLY";

    fn fixed() -> SignedHeaders {
        let now = DateTime::parse_from_rfc3339("2026-10-20T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        sign_at(
            "key-1",
            FIXED_KEY,
            "post",
            "/api/pdf-jobs/lease",
            r#"{"workerId":"pdf-worker-01"}"#,
            now,
            "8f14e45f-ceea-467a-9575-0b5f6d6c1f1a",
        )
    }

    /// The vector was produced independently with Python's hmac/hashlib over
    /// the same canonical string; the original TypeScript signer agreed with
    /// that reference on its own vector.
    #[test]
    fn matches_an_independent_reference_signer() {
        let h = fixed();
        assert_eq!(h.timestamp, "2026-10-20T12:34:56.789Z");
        assert_eq!(h.signature, REFERENCE_SIGNATURE);
    }

    const REFERENCE_SIGNATURE: &str = "c32df7bcdeb34ffddb4d7c51ef677474e4d782d492ed47ae8300f5d60ed7b479";

    #[test]
    fn method_case_does_not_matter_but_everything_else_does() {
        let base = fixed();
        let now = DateTime::parse_from_rfc3339("2026-10-20T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        let nonce = "8f14e45f-ceea-467a-9575-0b5f6d6c1f1a";
        let secret = FIXED_KEY;
        let body = r#"{"workerId":"pdf-worker-01"}"#;
        let upper = sign_at("key-1", secret, "POST", "/api/pdf-jobs/lease", body, now, nonce);
        assert_eq!(upper.signature, base.signature);
        for changed in [
            sign_at("key-1", secret, "post", "/api/pdf-jobs/heartbeat", body, now, nonce),
            sign_at("key-1", secret, "post", "/api/pdf-jobs/lease", "{}", now, nonce),
            sign_at(
                "key-1",
                secret,
                "post",
                "/api/pdf-jobs/lease",
                body,
                now,
                "another-nonce",
            ),
            sign_at(
                "key-1",
                "a-different-secret",
                "post",
                "/api/pdf-jobs/lease",
                body,
                now,
                nonce,
            ),
        ] {
            assert_ne!(changed.signature, base.signature);
        }
    }

    #[test]
    fn verify_accepts_its_own_signature_and_rejects_tampering() {
        let h = sign("key-1", SECRET, "PUT", "/api/pdf-jobs/42/complete", r#"{"ok":true}"#);
        let check = |body: &str, sig: &str| {
            verify(
                SECRET,
                "put",
                "/api/pdf-jobs/42/complete",
                body,
                &h.timestamp,
                &h.nonce,
                sig,
            )
        };
        assert!(check(r#"{"ok":true}"#, &h.signature));
        assert!(!check(r#"{"ok":false}"#, &h.signature));
        assert!(!check(r#"{"ok":true}"#, "not-hex"));
        assert!(!check(r#"{"ok":true}"#, &"0".repeat(64)));
        assert_eq!(h.pairs().map(|(k, _)| k)[0], "x-worker-key-id");
    }
}
