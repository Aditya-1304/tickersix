//! Read-only repository checks for credentials accidentally committed to source.
//!
//! Configuration names and placeholder values are allowed. The audit reports
//! only private-key material or non-placeholder values assigned to fields that
//! are explicitly labelled as API keys, private keys, secrets, or seed phrases.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

const PEM_BEGIN_MARKER: &str = "-----begin";
const PEM_PRIVATE_KEY_MARKER: &str = "private key-----";

const AUDIT_ROOTS: &[&str] = &[
    "backend",
    "crates",
    "programs",
    "app",
    "scripts",
    "docs",
    "fixtures",
    "README.md",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecurityFinding {
    pub path: String,
    pub line: usize,
    pub rule: String,
}

/// Scans the release-relevant repository roots without reading ignored build
/// output, deployment artifacts, or the Git database.
pub fn audit_repository(repository_root: impl AsRef<Path>) -> Result<Vec<SecurityFinding>, String> {
    let root = repository_root.as_ref();
    let mut files = Vec::new();
    for relative in AUDIT_ROOTS {
        collect_files(&root.join(relative), &mut files)?;
    }

    let mut findings = Vec::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("security audit path cannot be relativized: {error}"))?
            .display()
            .to_string();
        findings.extend(scan_source_text(&relative, &text));
    }
    Ok(findings)
}

/// Scans one UTF-8 source or documentation file. Keeping this function pure
/// makes the detection rules testable without creating a real credential file.
pub fn scan_source_text(path: &str, text: &str) -> Vec<SecurityFinding> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let lower = line.to_ascii_lowercase();
            let rule = if lower.contains(PEM_BEGIN_MARKER) && lower.contains(PEM_PRIVATE_KEY_MARKER)
            {
                Some("private_key_pem".to_owned())
            } else if has_credential_assignment(&lower) {
                Some("inline_credential_assignment".to_owned())
            } else {
                None
            }?;
            Some(SecurityFinding {
                path: path.to_owned(),
                line: index + 1,
                rule,
            })
        })
        .collect()
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if path.is_file() {
        files.push(path.to_owned());
        return Ok(());
    }
    if !path.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(path)
        .map_err(|error| format!("security audit cannot read {}: {error}", path.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("security audit directory entry failed: {error}"))?;
        let child = entry.path();
        if child
            .file_name()
            .is_some_and(|name| name == ".git" || name == "target")
        {
            continue;
        }
        collect_files(&child, files)?;
    }
    Ok(())
}

fn has_credential_assignment(line: &str) -> bool {
    const CREDENTIAL_LABELS: &[&str] = &[
        "api_key",
        "api-key",
        "private_key",
        "private-key",
        "secret_key",
        "secret-key",
        "seed_phrase",
        "seed-phrase",
        "mnemonic",
    ];

    for label in CREDENTIAL_LABELS {
        let Some(position) = line.find(label) else {
            continue;
        };
        let suffix = line[position + label.len()..].trim_start();
        let Some(value) = suffix
            .strip_prefix('=')
            .or_else(|| suffix.strip_prefix(':'))
        else {
            continue;
        };
        let raw_value = value.trim();
        if raw_value.starts_with("...") {
            continue;
        }
        let value = raw_value.trim_matches(|character: char| character.is_ascii_punctuation());
        if value.is_empty()
            || value == "..."
            || value.starts_with("...")
            || value == "null"
            || value == "none"
            || value == "string"
            || value.starts_with("env::")
            || value.starts_with("process.env")
            || value.starts_with("option")
            || value.starts_with("std::env")
            || value.starts_with("parse_")
            || value.starts_with("config.")
            || value.starts_with("raw.")
            || value.starts_with("[")
            || value.starts_with("&")
            || value.starts_with("vec")
        {
            continue;
        }
        if value.len() >= 20 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_credential_value_is_reported_but_configuration_name_is_allowed() {
        let leaked_value = String::from("012345678901234567") + "89";
        let source = format!(
            "const apiKey = process.env.TICKERSIX_JUPITER_API_KEY;\nconst leaked = {{ api_key: \"{leaked_value}\" }};"
        );
        let findings = scan_source_text("fixture.js", &source);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        assert_eq!(findings[0].rule, "inline_credential_assignment");
    }

    #[test]
    fn private_key_pem_material_is_reported_without_needing_a_field_name() {
        let findings = scan_source_text(
            "fixture.txt",
            &format!(
                "-----BEGIN {}-----\nredacted\n-----END {}-----",
                "PRIVATE KEY", "PRIVATE KEY"
            ),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "private_key_pem");
    }

    #[test]
    fn documented_placeholder_values_are_not_findings() {
        let placeholder_source = [
            "TICKERSIX_JUPITER_API_KEY=...",
            "secret_key_hex: String",
            "api_key: null",
        ]
        .join("\n");
        let findings = scan_source_text("README.md", &placeholder_source);

        assert!(findings.is_empty());
    }
}
