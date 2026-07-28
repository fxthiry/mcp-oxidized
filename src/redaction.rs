//! Centralized, line-oriented configuration secret redaction.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

pub const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct RedactionMetadata {
    pub enabled: bool,
    pub replacement_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RedactedText {
    pub text: String,
    pub redaction: RedactionMetadata,
}

// Each expression captures the non-secret prefix and optional harmless suffix.
// Matching is anchored so prose and identifiers that merely contain words such
// as "password" are not masked.
static SECRET_LINES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#"(?i)^(\s*snmp-server\s+community\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*(?:username\s+\S+(?:\s+privilege\s+\d+)?(?:\s+view\s+\S+)?|enable)\s+(?:password|secret)\s+(?:\d+\s+)?)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*(?:login\s+)?password\s+(?:\d+\s+)?)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*(?:tacacs-server|radius-server)\s+(?:host\s+\S+\s+)?key\s+(?:\d+\s+)?)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*key(?:-string)?\s+(?:\d+\s+)?)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*neighbor\s+\S+\s+password\s+(?:\d+\s+)?)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*ip\s+(?:ospf|rip|eigrp).*(?:authentication-key|message-digest-key\s+\d+\s+md5)\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*crypto\s+isakmp\s+key\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*set\s+snmp\s+community\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*set\s+system\s+(?:root-authentication|login\s+user\s+\S+\s+authentication)\s+(?:encrypted-password|plain-text-password)\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*set\s+system\s+(?:tacplus-server|radius-server)\s+\S+\s+secret\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*set\s+protocols\s+(?:bgp|ospf).*\s+authentication-key\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*set\s+security\s+ike\s+policy\s+\S+\s+pre-shared-key\s+(?:ascii-text|hexadecimal)\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*(?:encrypted-password|plain-text-password|secret|authentication-key)\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*pre-shared-key(?:\s+(?:ascii-text|hexadecimal))?\s+)(?:"[^"]*"|\S+)(.*)$"#,
        r#"(?i)^(\s*!?\s*(?:#\s*)?(?:oxidized\s+)?secret-data\b\s*[:=]?\s*)(.*)$"#,
        r#"^(\s*)[A-Za-z0-9+/]{40,}={0,2}(\s*)$"#,
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("constant redaction regex is valid"))
    .collect()
});

static PRIVATE_KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----\s*$")
        .expect("private key begin regex is valid")
});
static PRIVATE_KEY_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*-----END (?:[A-Z0-9 ]+ )?PRIVATE KEY-----\s*$")
        .expect("private key end regex is valid")
});
static JUNOS_SNMP_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*snmp\s*\{").expect("constant regex is valid"));
static JUNOS_SNMP_COMMUNITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^(\s*community\s+)(?:"[^"]*"|\S+)(\s*\{.*)$"#)
        .expect("constant regex is valid")
});

pub fn redact(text: &str, enabled: bool) -> RedactedText {
    redact_impl(text, enabled, true)
}

/// Redact a sequence while keeping one output line for every input line.
/// This is used by diffs, where collapsing a private-key block would destroy
/// the relationship between change markers and line numbers.
pub fn redact_preserving_lines(text: &str, enabled: bool) -> RedactedText {
    redact_impl(text, enabled, false)
}

fn redact_impl(text: &str, enabled: bool, collapse_private_keys: bool) -> RedactedText {
    if !enabled {
        return RedactedText {
            text: text.to_string(),
            redaction: RedactionMetadata {
                enabled: false,
                replacement_count: 0,
            },
        };
    }

    let mut output = Vec::new();
    let mut replacement_count = 0usize;
    let mut in_private_key = false;
    let mut brace_depth = 0isize;
    let mut junos_snmp_depth = None;

    for line in text.lines() {
        if PRIVATE_KEY_BEGIN.is_match(line) {
            in_private_key = true;
            replacement_count += 1;
            output.push(REDACTED.to_string());
            continue;
        }
        if in_private_key {
            if PRIVATE_KEY_END.is_match(line) {
                in_private_key = false;
            }
            if !collapse_private_keys {
                output.push(REDACTED.to_string());
            }
            continue;
        }

        if JUNOS_SNMP_START.is_match(line) {
            junos_snmp_depth = Some(brace_depth + 1);
        }
        let replacement = junos_snmp_depth
            .and_then(|_| {
                JUNOS_SNMP_COMMUNITY.captures(line).map(|captures| {
                    format!(
                        "{}{}{}",
                        captures.get(1).map_or("", |capture| capture.as_str()),
                        REDACTED,
                        captures.get(2).map_or("", |capture| capture.as_str())
                    )
                })
            })
            .or_else(|| {
                SECRET_LINES.iter().find_map(|regex| {
                    regex.captures(line).map(|captures| {
                        let prefix = captures.get(1).map_or("", |capture| capture.as_str());
                        let suffix = captures.get(2).map_or("", |capture| capture.as_str());
                        format!("{prefix}{REDACTED}{suffix}")
                    })
                })
            });

        if let Some(replacement) = replacement {
            replacement_count += 1;
            output.push(replacement);
        } else {
            output.push(line.to_string());
        }

        brace_depth += line.matches('{').count() as isize;
        brace_depth -= line.matches('}').count() as isize;
        if junos_snmp_depth.is_some_and(|depth| brace_depth < depth) {
            junos_snmp_depth = None;
        }
    }

    let mut redacted = output.join("\n");
    if text.ends_with('\n') {
        redacted.push('\n');
    }

    RedactedText {
        text: redacted,
        redaction: RedactionMetadata {
            enabled: true,
            replacement_count,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_cisco_and_junos_secrets_but_not_lookalikes() {
        let input = concat!(
            "snmp-server community public ro\n",
            "username admin secret 9 hash\n",
            "set snmp community private authorization read-only\n",
            "set system root-authentication encrypted-password \"$6$hash\"\n",
            "snmp {\n",
            "  community hierarchical-private {\n",
            "    authorization read-only;\n",
            "  }\n",
            "}\n",
            "authentication-key \"$9$routing-secret\";\n",
            "pre-shared-key ascii-text \"$9$vpn-secret\";\n",
            "policy-options {\n",
            "  community harmless-community members 64512:10;\n",
            "}\n",
            "description password rotation link\n",
        );
        let result = redact(input, true);

        assert_eq!(result.redaction.replacement_count, 7);
        assert!(!result.text.contains("public"));
        assert!(!result.text.contains("$6$hash"));
        assert!(!result.text.contains("hierarchical-private"));
        assert!(!result.text.contains("routing-secret"));
        assert!(!result.text.contains("vpn-secret"));
        assert!(result.text.contains("harmless-community"));
        assert!(result.text.contains("description password rotation link"));
    }

    #[test]
    fn removes_complete_private_key_blocks() {
        let input =
            "before\n-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\nafter\n";
        let result = redact(input, true);
        assert_eq!(result.text, "before\n<redacted>\nafter\n");
        assert_eq!(result.redaction.replacement_count, 1);
    }

    #[test]
    fn disabled_returns_raw_text() {
        let result = redact("enable secret raw\n", false);
        assert_eq!(result.text, "enable secret raw\n");
        assert_eq!(result.redaction.replacement_count, 0);
        assert!(!result.redaction.enabled);
    }
}
