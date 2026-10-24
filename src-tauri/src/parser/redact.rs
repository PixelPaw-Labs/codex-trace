//! Display-time secret redaction for exec command text.
//!
//! Codex v0.147.0 (#36893, #36908) started redacting bearer tokens and other secrets from
//! commands rendered by its own app-server-protocol layer (`ThreadItem::CommandExecution`,
//! used by IDE/app clients for live and replayed history). That redaction happens only when
//! building those client-facing items — the raw, unredacted `command: Vec<String>` is still
//! what gets persisted to the rollout JSONL that codex-trace reads directly. Without its own
//! redaction, codex-trace would surface secrets that Codex's own UIs now hide. These regexes
//! mirror `codex-rs/secrets/src/sanitizer.rs` so codex-trace redacts the same patterns Codex
//! itself does.
//!
//! Codex v0.164.0 (#54309, #54432, #56049, issue #335) adds a different mechanism for remote
//! Bash execution and Zsh shell snapshot sessions: a network-proxy credential broker
//! (`codex-rs/network-proxy/src/credential_broker/`) that swaps the real credential for a
//! shaped placeholder — `<prefix>masked<random alphanumeric tail>`, built by
//! `providers::shaped_dummy_value` — in the command's own environment *before* the command or
//! its output is generated at all. So the rollout JSONL for these sessions never contains the
//! raw secret in the first place; it contains this placeholder, for built-in providers
//! (OpenAI's `sk-`/`sk-proj-`/`sk-svcacct-`/`sk-admin-`, GitHub's `ghp_`/`gho_`/`ghu_`/`ghs_`/
//! `ghr_`/`github_pat_`) and arbitrary org-configured custom providers alike. The per-provider
//! regexes below can't enumerate every custom-provider prefix, so `masked_credential_marker_regex`
//! recognizes the placeholder's shape instead of its prefix.
use regex::Regex;
use std::sync::OnceLock;

fn openai_key_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // Project-scoped keys (`sk-proj-...`) embed hyphens in the body, unlike legacy
    // `sk-...` keys — without `_-` in the class, codex-trace never matched them at all.
    REGEX.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9_-]{20,}").expect("valid regex"))
}

fn masked_credential_marker_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // Matches Codex's own `<prefix>masked<tail>` placeholder (see module docs): a short
    // identifier-like prefix immediately followed by the literal "masked" and a long
    // alphanumeric tail. `shaped_dummy_value` guarantees at least 22 random characters
    // after the marker (reserving >128 bits of entropy), so requiring 20+ here matches
    // every real placeholder while keeping false positives on ordinary prose effectively
    // impossible — plain text essentially never has 20+ unbroken alphanumeric characters
    // immediately after the word "masked".
    REGEX.get_or_init(|| {
        Regex::new(r"[A-Za-z][A-Za-z0-9_-]{0,24}masked[A-Za-z0-9]{20,}").expect("valid regex")
    })
}

fn aws_access_key_id_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid regex"))
}

fn bearer_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i:\bBearer)[ \t]+[A-Za-z0-9._~+/-]{16,}=*").expect("valid regex")
    })
}

fn secret_assignment_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#)
            .expect("valid regex")
    })
}

/// Best-effort redaction of recognizable secrets (bearer tokens, OpenAI/AWS-style keys,
/// and `key=value`/`key: value` secret assignments) from a single line of display text.
pub fn redact_secrets(input: &str) -> String {
    let redacted = bearer_token_regex().replace_all(input, "Bearer [REDACTED_SECRET]");
    let redacted = masked_credential_marker_regex().replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = openai_key_regex().replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = aws_access_key_id_regex().replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = secret_assignment_regex().replace_all(&redacted, "$1$2$3[REDACTED_SECRET]");
    redacted.into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;

    // Fixture tokens are assembled from split literals at runtime so the source never
    // contains a single string matching a real credential pattern.
    fn fixture_token(prefix: &str, suffix: &str) -> String {
        format!("{prefix}{suffix}")
    }

    #[test]
    fn redacts_bearer_token() {
        let token = fixture_token("abcdefghijklmnop", "01234567");
        let input = format!("curl -H \"Authorization: Bearer {token}\" https://api.example.com");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("Bearer [REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_openai_style_key() {
        let token = fixture_token("sk-", "abcdefghijklmnopqrstuvwx");
        let input = format!("export OPENAI_API_KEY={token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_openai_project_scoped_key_with_hyphens() {
        let token = fixture_token(
            "sk-proj-",
            "executor-only-0000000000000000000000000000000000",
        );
        let output = redact_secrets(&token);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    // Codex v0.164.0 (#54309, #54432, #56049, issue #335): remote Bash/Zsh-snapshot exec
    // sessions never carry the raw secret — the network-proxy credential broker swaps it for
    // a shaped `<prefix>masked<tail>` placeholder before the command/output is generated.
    // codex-trace must recognize that marker shape even for prefixes it has no dedicated
    // regex for (GitHub tokens, arbitrary custom/configured providers), or the placeholder
    // shows up as unflagged, malformed-looking text instead of `[REDACTED_SECRET]`.
    #[test]
    fn redacts_github_style_masked_credential_placeholder() {
        let token = fixture_token("ghp_masked", "placeholderonly000000000000000000000");
        let input = format!("export GITHUB_TOKEN={token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_custom_provider_masked_credential_placeholder() {
        let token = fixture_token("acme-key-masked", "placeholderonly000000000000000000000");
        let input = format!("curl -H \"X-Acme-Key: {token}\" https://api.example.com");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn leaves_the_word_masked_alone_without_a_long_alphanumeric_tail() {
        let input = "the api key was masked for this session";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redacts_aws_access_key_id() {
        let token = fixture_token("AKIA", "ABCDEFGHIJKLMNOP");
        let input = format!("aws configure set aws_access_key_id {token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_generic_secret_assignment() {
        let token = fixture_token("supersecretvalue", "123");
        let input = format!("--password={token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn leaves_ordinary_commands_untouched() {
        let input = "npx oxlint src/components/ToolCallItem.tsx";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn avoids_bearer_false_positive_on_short_word() {
        let input = "Bearer of good news arrived";
        assert_eq!(redact_secrets(input), input);
    }
}
