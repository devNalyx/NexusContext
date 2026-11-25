use regex::Regex;
use std::sync::OnceLock;

/// Broad/aggressive by design (a deliberate choice, not a default worth
/// softening quietly): this trades some collateral damage on legitimate
/// content (a real hash, a real IP in a config example) for catching more
/// of the highest-severity leak class. It is NOT comprehensive PII
/// scrubbing — it targets known secret shapes and generic secret-looking
/// assignments, nothing that requires understanding meaning.
///
/// Applied before anything derived from a transcript is ever stored -
/// see `crate::staging`, the only caller that should ever see raw
/// transcript text.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (pattern, replacement) in patterns() {
        out = pattern.replace_all(&out, *replacement).into_owned();
    }
    redact_high_entropy_tokens(&out)
}

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Private key blocks (multi-line) - must run before shorter
            // patterns that could otherwise partially match inside one.
            (
                Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
                    .unwrap(),
                "[REDACTED:private_key]",
            ),
            (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "[REDACTED:aws_key]"),
            (
                Regex::new(r"eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}").unwrap(),
                "[REDACTED:jwt]",
            ),
            (
                Regex::new(r"(?i)Bearer\s+[A-Za-z0-9\-_.=]{8,}").unwrap(),
                "Bearer [REDACTED:bearer_token]",
            ),
            // Generic secret-looking assignment: KEY=value, "token": "value",
            // password: value, etc. Keeps the variable name, drops the value.
            (
                Regex::new(
                    r#"(?i)([A-Za-z0-9_]*(?:key|secret|token|password|passwd|credential)[A-Za-z0-9_]*)\s*[:=]\s*['"]?[^\s'",;]{4,}['"]?"#,
                )
                .unwrap(),
                "$1=[REDACTED:secret_assignment]",
            ),
            (
                Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
                "[REDACTED:email]",
            ),
            (
                Regex::new(r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b").unwrap(),
                "[REDACTED:ip]",
            ),
        ]
    })
}

/// Long, high-entropy-looking tokens (raw API keys/secrets that don't
/// match a known structured format) get caught here as a fallback.
/// Shannon entropy over the byte distribution - crude, but cheap and
/// dependency-free. Threshold of 3.5 is deliberately below a 16-symbol
/// hex alphabet's max of exactly 4.0 bits/char (a real hex secret scores
/// ~3.9-4.0) while staying above typical English-word entropy (~2.5-3.0
/// bits/char even for long words) - measured, not guessed.
fn redact_high_entropy_tokens(text: &str) -> String {
    static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    let token_re = TOKEN_RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_=-]{20,}").unwrap());

    token_re
        .replace_all(text, |caps: &regex::Captures| {
            let token = &caps[0];
            if shannon_entropy(token) > 3.5 {
                "[REDACTED:high_entropy]".to_string()
            } else {
                token.to_string()
            }
        })
        .into_owned()
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    let mut total = 0u32;
    for byte in s.bytes() {
        counts[byte as usize] += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_private_key_blocks() {
        let input = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIExamplePretendKeyBase64Content\n-----END RSA PRIVATE KEY-----\nafter";
        let out = redact(input);
        assert!(out.contains("[REDACTED:private_key]"));
        assert!(!out.contains("Pretend"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn redacts_aws_access_key() {
        let out = redact("aws_access_key_id = AKIAIOSFODNN7EXAMPLE");
        assert!(out.contains("[REDACTED"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn redacts_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let out = redact(&format!("Authorization header was {jwt}"));
        assert!(out.contains("[REDACTED:jwt]"));
        assert!(!out.contains(jwt));
    }

    #[test]
    fn redacts_bearer_token() {
        let out = redact("curl -H \"Authorization: Bearer sk-abcdef1234567890\"");
        assert!(out.contains("Bearer [REDACTED:bearer_token]"));
        assert!(!out.contains("sk-abcdef1234567890"));
    }

    #[test]
    fn redacts_generic_secret_assignments() {
        let out = redact("DATABASE_PASSWORD=hunter2verysecret and API_TOKEN: \"abc123xyz789\"");
        assert!(!out.contains("hunter2verysecret"));
        assert!(!out.contains("abc123xyz789"));
        assert!(out.contains("DATABASE_PASSWORD"));
        assert!(out.contains("API_TOKEN"));
    }

    #[test]
    fn redacts_emails_and_ips() {
        let out = redact("contact me at dev@example.com or ssh into 10.0.0.42");
        assert!(out.contains("[REDACTED:email]"));
        assert!(out.contains("[REDACTED:ip]"));
        assert!(!out.contains("dev@example.com"));
        assert!(!out.contains("10.0.0.42"));
    }

    #[test]
    fn redacts_high_entropy_looking_tokens() {
        // No "key/secret/token/password" keyword nearby - this must be
        // caught by entropy alone, not the labeled-assignment pattern.
        let out = redact("the deploy id was 9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f0a");
        assert!(out.contains("[REDACTED:high_entropy]"));
    }

    #[test]
    fn leaves_ordinary_low_entropy_text_alone() {
        let out = redact("run migrate.sh then restart the service and verify health");
        assert_eq!(
            out,
            "run migrate.sh then restart the service and verify health"
        );
    }
}
