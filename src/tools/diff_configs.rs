//! Tool to compare two configuration versions of a node (FR9).
//!
//! Provides LLM-friendly diff output with structured sections for additions,
//! deletions, and modifications using the Myers diff algorithm via the `similar` crate.
//!
//! # Example
//!
//! ```ignore
//! use mcp_oxidized::tools::diff_configs;
//! use mcp_oxidized::oxidized::OxidizedClient;
//!
//! let result = diff_configs(&client, "SW-Core-01", "abc123", "def456").await?;
//! println!("{}", result.to_llm_format());
//! ```

use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use tracing::instrument;

use crate::error::OxidizedError;
use crate::oxidized::{OxidizedBackend, OxidizedClient};
use crate::redaction::{RedactionMetadata, redact_preserving_lines};

use super::enrich_node_not_found;

// ============================================================================
// Diff Result Types
// ============================================================================

/// A single line change in the diff.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LineChange {
    /// Line number in the source version
    pub line_num: usize,
    /// The content of the line (without trailing newline)
    pub content: String,
}

/// A modification where a line changed between versions.
///
/// Represents a contiguous block of changes where lines were replaced.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Modification {
    /// Starting line number in version 1
    pub v1_line_start: usize,
    /// Ending line number in version 1 (inclusive)
    pub v1_line_end: usize,
    /// Starting line number in version 2
    pub v2_line_start: usize,
    /// Ending line number in version 2 (inclusive)
    pub v2_line_end: usize,
    /// Original content lines from version 1
    pub old_content: Vec<String>,
    /// New content lines in version 2
    pub new_content: Vec<String>,
}

/// Summary statistics for the diff.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct DiffSummary {
    /// Number of lines added
    pub lines_added: usize,
    /// Number of lines removed
    pub lines_removed: usize,
    /// Number of modification blocks (hunks)
    pub modification_blocks: usize,
}

/// Result of comparing two configuration versions.
///
/// Contains structured diff information optimized for LLM consumption.
#[derive(Debug, Clone, Serialize)]
pub struct DiffResult {
    /// Node name
    pub node: String,
    /// Version 1 OID (older version)
    pub version1: String,
    /// Version 2 OID (newer version)
    pub version2: String,
    /// Whether the configurations are identical
    pub identical: bool,
    /// Summary statistics
    pub summary: DiffSummary,
    /// Lines added in version 2 (pure insertions, not part of modifications)
    pub additions: Vec<LineChange>,
    /// Lines removed from version 1 (pure deletions, not part of modifications)
    pub deletions: Vec<LineChange>,
    /// Modification blocks where lines were replaced
    pub modifications: Vec<Modification>,
    /// Unified diff output for raw access
    pub unified_diff: String,
    pub redaction: RedactionMetadata,
}

impl DiffResult {
    /// Format the diff result as an LLM-friendly string (FR9).
    ///
    /// Output format is structured for easy parsing by AI assistants:
    /// - Summary section with statistics
    /// - Unified diff for complete context
    /// - Structured changes sections when helpful
    ///
    /// # Example Output
    ///
    /// ```text
    /// ## Configuration Diff: SW-Core-01
    /// Comparing version abc123 to def456
    ///
    /// ### Summary
    /// - Lines added: 5
    /// - Lines removed: 2
    /// - Modification blocks: 1
    ///
    /// ### Unified Diff
    /// ```diff
    /// @@ -10,3 +10,5 @@
    ///  interface GigabitEthernet0/1
    /// -  ip address 10.0.0.1 255.255.255.0
    /// +  ip address 10.0.0.2 255.255.255.0
    /// +  description Updated link
    /// ```
    pub fn to_llm_format(&self) -> String {
        let mut output = String::new();

        // Header
        output.push_str(&format!("## Configuration Diff: {}\n", self.node));
        output.push_str(&format!(
            "Comparing version {} to {}\n\n",
            self.version1, self.version2
        ));

        // Identical check
        if self.identical {
            output.push_str("### Result\nConfigurations are identical. No changes detected.\n");
            return output;
        }

        // Summary section
        output.push_str("### Summary\n");
        output.push_str(&format!("- Lines added: {}\n", self.summary.lines_added));
        output.push_str(&format!(
            "- Lines removed: {}\n",
            self.summary.lines_removed
        ));
        output.push_str(&format!(
            "- Modification blocks: {}\n\n",
            self.summary.modification_blocks
        ));

        // Unified diff section (the most useful for LLMs)
        output.push_str("### Unified Diff\n```diff\n");
        output.push_str(&self.unified_diff);
        if !self.unified_diff.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("```\n");

        // Structured additions (if any pure additions outside modifications)
        if !self.additions.is_empty() {
            output.push_str("\n### Pure Additions\n");
            for change in &self.additions {
                output.push_str(&format!(
                    "+ [line {}] {}\n",
                    change.line_num, change.content
                ));
            }
        }

        // Structured deletions (if any pure deletions outside modifications)
        if !self.deletions.is_empty() {
            output.push_str("\n### Pure Deletions\n");
            for change in &self.deletions {
                output.push_str(&format!(
                    "- [line {}] {}\n",
                    change.line_num, change.content
                ));
            }
        }

        output
    }
}

// ============================================================================
// Diff Algorithm (using similar crate - Myers algorithm)
// ============================================================================

/// Compute a diff between two configuration strings using the Myers algorithm.
///
/// Uses the `similar` crate which implements efficient LCS-based diffing,
/// the same algorithm used by git and other professional diff tools.
///
/// # Arguments
///
/// * `config1` - The first configuration (older version)
/// * `config2` - The second configuration (newer version)
///
/// # Returns
///
/// A tuple of (additions, deletions, modifications, summary, unified_diff)
pub fn compute_diff(
    config1: &str,
    config2: &str,
) -> (
    Vec<LineChange>,
    Vec<LineChange>,
    Vec<Modification>,
    DiffSummary,
    String,
) {
    // Create the diff using similar's TextDiff with line-level granularity
    let diff = TextDiff::from_lines(config1, config2);

    let mut additions = Vec::new();
    let mut deletions = Vec::new();
    let mut modifications = Vec::new();

    let mut lines_added = 0usize;
    let mut lines_removed = 0usize;

    // Track current modification block
    let mut current_mod: Option<(usize, usize, Vec<String>, Vec<String>)> = None;

    // Process each change
    for change in diff.iter_all_changes() {
        let line_content = change.value().trim_end_matches('\n').to_string();

        match change.tag() {
            ChangeTag::Delete => {
                lines_removed += 1;
                let old_line = change.old_index().unwrap_or(0) + 1;

                // Check if we should extend current modification or start new one
                if let Some((_mod_start, _, ref mut old_lines, ref new_lines)) = current_mod {
                    // Continue modification block
                    old_lines.push(line_content);
                    // Update the modification start if needed
                    if new_lines.is_empty() {
                        // Still collecting deletions
                    }
                } else {
                    // Start a potential modification block
                    current_mod = Some((old_line, 0, vec![line_content], vec![]));
                }
            }
            ChangeTag::Insert => {
                lines_added += 1;
                let new_line = change.new_index().unwrap_or(0) + 1;

                if let Some((mod_start, _, ref old_lines, ref mut new_lines)) = current_mod {
                    // We have pending deletions, this is a modification
                    if new_lines.is_empty() {
                        current_mod =
                            Some((mod_start, new_line, old_lines.clone(), vec![line_content]));
                    } else {
                        new_lines.push(line_content);
                    }
                } else {
                    // Pure addition (no preceding deletion)
                    additions.push(LineChange {
                        line_num: new_line,
                        content: line_content,
                    });
                }
            }
            ChangeTag::Equal => {
                // Flush any pending modification
                if let Some((v1_start, v2_start, old_lines, new_lines)) = current_mod.take() {
                    if !new_lines.is_empty() {
                        // It's a modification (had both deletions and insertions)
                        modifications.push(Modification {
                            v1_line_start: v1_start,
                            v1_line_end: v1_start + old_lines.len().saturating_sub(1),
                            v2_line_start: v2_start,
                            v2_line_end: v2_start + new_lines.len().saturating_sub(1),
                            old_content: old_lines,
                            new_content: new_lines,
                        });
                    } else {
                        // Pure deletions (no insertions followed)
                        for (i, content) in old_lines.into_iter().enumerate() {
                            deletions.push(LineChange {
                                line_num: v1_start + i,
                                content,
                            });
                        }
                    }
                }
            }
        }
    }

    // Flush any remaining modification at end of file
    if let Some((v1_start, v2_start, old_lines, new_lines)) = current_mod.take() {
        if !new_lines.is_empty() {
            modifications.push(Modification {
                v1_line_start: v1_start,
                v1_line_end: v1_start + old_lines.len().saturating_sub(1),
                v2_line_start: v2_start,
                v2_line_end: v2_start + new_lines.len().saturating_sub(1),
                old_content: old_lines,
                new_content: new_lines,
            });
        } else {
            for (i, content) in old_lines.into_iter().enumerate() {
                deletions.push(LineChange {
                    line_num: v1_start + i,
                    content,
                });
            }
        }
    }

    // Generate unified diff string
    let unified_diff = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("version {}", "1"), &format!("version {}", "2"))
        .to_string();

    let summary = DiffSummary {
        lines_added,
        lines_removed,
        modification_blocks: modifications.len(),
    };

    (additions, deletions, modifications, summary, unified_diff)
}

// ============================================================================
// Tool Entry Point
// ============================================================================

/// Compare two configuration versions of a node (FR9).
///
/// Fetches both versions concurrently and computes a diff using the Myers algorithm.
/// Returns structured output optimized for LLM consumption.
///
/// # Arguments
///
/// * `backend` - The Oxidized client to use
/// * `node` - The node name
/// * `version1` - The first version OID (older)
/// * `version2` - The second version OID (newer)
///
/// # Returns
///
/// A `DiffResult` containing the diff with structured sections.
///
/// # Errors
///
/// - [`OxidizedError::NodeNotFound`] - Node or version does not exist (includes suggestions)
/// - [`OxidizedError::ApiUnreachable`] - Network/connection error
///
/// # Example
///
/// ```ignore
/// let result = diff_configs(&client, "SW-Core-01", "abc123", "def456").await?;
/// println!("{}", result.to_llm_format());
/// ```
#[instrument(skip(backend), fields(node = %node, version1 = %version1, version2 = %version2))]
pub async fn diff_configs(
    backend: &OxidizedClient,
    node: &str,
    version1: &str,
    version2: &str,
) -> Result<DiffResult, OxidizedError> {
    // Fetch both versions concurrently for performance
    let (config1_result, config2_result) = tokio::join!(
        backend.get_node_version(node, version1),
        backend.get_node_version(node, version2)
    );

    // Handle errors with enriched suggestions
    let config1 = match config1_result {
        Ok(c) => c,
        Err(OxidizedError::NodeNotFound(node_name, _)) => {
            return Err(enrich_node_not_found(backend, node_name).await);
        }
        Err(e) => return Err(e),
    };

    let config2 = match config2_result {
        Ok(c) => c,
        Err(OxidizedError::NodeNotFound(node_name, _)) => {
            return Err(enrich_node_not_found(backend, node_name).await);
        }
        Err(e) => return Err(e),
    };

    // Check if identical
    if config1 == config2 {
        tracing::info!(node = %node, "Configurations are identical");
        return Ok(DiffResult {
            node: node.to_string(),
            version1: version1.to_string(),
            version2: version2.to_string(),
            identical: true,
            summary: DiffSummary::default(),
            additions: vec![],
            deletions: vec![],
            modifications: vec![],
            unified_diff: String::new(),
            redaction: RedactionMetadata {
                enabled: backend.redaction_enabled(),
                replacement_count: 0,
            },
        });
    }

    // Compute diff using Myers algorithm
    let (mut additions, mut deletions, mut modifications, summary, mut unified_diff) =
        compute_diff(&config1, &config2);
    let redaction_enabled = backend.redaction_enabled();
    let mut replacement_count = 0usize;
    replacement_count += redact_line_changes(&mut additions, redaction_enabled);
    replacement_count += redact_line_changes(&mut deletions, redaction_enabled);
    for modification in &mut modifications {
        replacement_count += redact_string_lines(&mut modification.old_content, redaction_enabled);
        replacement_count += redact_string_lines(&mut modification.new_content, redaction_enabled);
    }
    let redacted = redact_unified_diff(&unified_diff, redaction_enabled);
    unified_diff = redacted.0;
    replacement_count += redacted.1;

    tracing::info!(
        node = %node,
        added = summary.lines_added,
        removed = summary.lines_removed,
        mod_blocks = summary.modification_blocks,
        "Diff computed successfully"
    );

    Ok(DiffResult {
        node: node.to_string(),
        version1: version1.to_string(),
        version2: version2.to_string(),
        identical: false,
        summary,
        additions,
        deletions,
        modifications,
        unified_diff,
        redaction: RedactionMetadata {
            enabled: redaction_enabled,
            replacement_count,
        },
    })
}

fn redact_line_changes(changes: &mut [LineChange], enabled: bool) -> usize {
    let mut contents: Vec<String> = changes
        .iter()
        .map(|change| change.content.clone())
        .collect();
    let count = redact_string_lines(&mut contents, enabled);
    for (change, content) in changes.iter_mut().zip(contents) {
        change.content = content;
    }
    count
}

fn redact_string_lines(lines: &mut [String], enabled: bool) -> usize {
    let redacted = redact_preserving_lines(&lines.join("\n"), enabled);
    for (line, content) in lines.iter_mut().zip(redacted.text.lines()) {
        *line = content.to_string();
    }
    redacted.redaction.replacement_count
}

fn redact_unified_diff(diff: &str, enabled: bool) -> (String, usize) {
    let marked: Vec<(String, &str)> = diff
        .lines()
        .map(|line| match line.chars().next() {
            Some(marker @ ('+' | '-' | ' ')) => (marker.to_string(), &line[marker.len_utf8()..]),
            _ => (String::new(), line),
        })
        .collect();
    let contents = marked
        .iter()
        .map(|(_, content)| *content)
        .collect::<Vec<_>>()
        .join("\n");
    let redacted = redact_preserving_lines(&contents, enabled);
    let lines: Vec<String> = marked
        .into_iter()
        .zip(redacted.text.lines())
        .map(|((marker, _), content)| format!("{marker}{content}"))
        .collect();
    let mut output = lines.join("\n");
    if diff.ends_with('\n') {
        output.push('\n');
    }
    (output, redacted.redaction.replacement_count)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Diff Algorithm Tests (using similar crate)
    // -------------------------------------------------------------------------

    #[test]
    fn test_compute_diff_identical_configs() {
        let config = "line1\nline2\nline3";
        let (additions, deletions, modifications, summary, unified) = compute_diff(config, config);

        assert!(additions.is_empty());
        assert!(deletions.is_empty());
        assert!(modifications.is_empty());
        assert_eq!(summary.lines_added, 0);
        assert_eq!(summary.lines_removed, 0);
        assert_eq!(summary.modification_blocks, 0);
        // Unified diff should be empty or minimal for identical files
        assert!(
            unified.is_empty() || !unified.contains('-') && !unified.contains('+'),
            "Identical files should have no changes in unified diff"
        );
    }

    #[test]
    fn test_compute_diff_additions_only() {
        let config1 = "line1\nline2\n";
        let config2 = "line1\nline2\nline3\nline4\n";

        let (additions, deletions, modifications, summary, unified) =
            compute_diff(config1, config2);

        assert!(summary.lines_added >= 2, "Should add at least 2 lines");
        assert_eq!(summary.lines_removed, 0);
        assert!(deletions.is_empty());
        // The new lines should appear either as pure additions or in a modification
        let total_new_lines = additions.len()
            + modifications
                .iter()
                .map(|m| m.new_content.len())
                .sum::<usize>();
        assert!(total_new_lines >= 2, "Should have at least 2 new lines");

        // Unified diff should show the additions
        assert!(unified.contains('+'), "Unified diff should show additions");
    }

    #[test]
    fn test_compute_diff_deletions_only() {
        let config1 = "line1\nline2\nline3\nline4\n";
        let config2 = "line1\nline2\n";

        let (additions, deletions, modifications, summary, unified) =
            compute_diff(config1, config2);

        assert!(summary.lines_removed >= 2, "Should remove at least 2 lines");
        assert_eq!(summary.lines_added, 0);
        assert!(additions.is_empty());
        // The removed lines should appear as deletions
        let total_removed = deletions.len()
            + modifications
                .iter()
                .map(|m| m.old_content.len())
                .sum::<usize>();
        assert!(total_removed >= 2, "Should have at least 2 removed lines");

        // Unified diff should show the deletions
        assert!(unified.contains('-'), "Unified diff should show deletions");
    }

    #[test]
    fn test_compute_diff_modifications() {
        let config1 = "hostname SW-01\ninterface Gi0/1\n  ip address 10.0.0.1";
        let config2 = "hostname SW-01\ninterface Gi0/1\n  ip address 10.0.0.2";

        let (_additions, _deletions, modifications, summary, unified) =
            compute_diff(config1, config2);

        // Should detect the IP address change
        assert!(
            summary.lines_added >= 1 || !modifications.is_empty(),
            "Should detect the IP change"
        );
        assert!(
            summary.lines_removed >= 1 || !modifications.is_empty(),
            "Should detect the IP change"
        );

        // Unified diff should show both old and new IP
        assert!(
            unified.contains("10.0.0.1") || unified.contains("10.0.0.2"),
            "Unified diff should reference the IP addresses"
        );
    }

    #[test]
    fn test_compute_diff_mixed_changes() {
        let config1 = "line1\nold_line\nline3";
        let config2 = "line1\nnew_line\nline3\nline4";

        let (additions, _deletions, modifications, summary, _unified) =
            compute_diff(config1, config2);

        // old_line -> new_line should be detected
        // line4 is an addition
        assert!(
            summary.lines_added >= 1,
            "Should detect at least one addition"
        );

        // Either detected as modification or as separate add/delete
        let has_changes =
            !modifications.is_empty() || !additions.is_empty() || summary.lines_removed > 0;
        assert!(has_changes, "Should detect the changes");
    }

    #[test]
    fn test_compute_diff_empty_configs() {
        let (additions, deletions, modifications, summary, _unified) = compute_diff("", "");

        assert!(additions.is_empty());
        assert!(deletions.is_empty());
        assert!(modifications.is_empty());
        assert_eq!(summary, DiffSummary::default());
    }

    #[test]
    fn test_compute_diff_empty_to_content() {
        let config2 = "line1\nline2";
        let (additions, deletions, _modifications, summary, unified) = compute_diff("", config2);

        assert_eq!(summary.lines_added, 2);
        assert!(deletions.is_empty());
        assert!(
            additions.len() >= 2 || summary.lines_added == 2,
            "Should add 2 lines"
        );
        assert!(unified.contains('+'), "Should show additions in unified");
    }

    #[test]
    fn test_compute_diff_content_to_empty() {
        let config1 = "line1\nline2";
        let (additions, deletions, _modifications, summary, unified) = compute_diff(config1, "");

        assert_eq!(summary.lines_removed, 2);
        assert!(additions.is_empty());
        assert!(
            deletions.len() >= 2 || summary.lines_removed == 2,
            "Should remove 2 lines"
        );
        assert!(unified.contains('-'), "Should show deletions in unified");
    }

    #[test]
    fn test_compute_diff_real_network_config() {
        // Test with realistic network configuration changes
        let config1 = r#"!
hostname SW-Core-01
!
interface GigabitEthernet0/1
  description Uplink to Router
  ip address 192.168.1.1 255.255.255.0
  no shutdown
!
interface GigabitEthernet0/2
  description Server Farm
  ip address 10.0.0.1 255.255.255.0
  no shutdown
!
end"#;

        let config2 = r#"!
hostname SW-Core-01
!
interface GigabitEthernet0/1
  description Uplink to Router-New
  ip address 192.168.1.2 255.255.255.0
  no shutdown
!
interface GigabitEthernet0/2
  description Server Farm
  ip address 10.0.0.1 255.255.255.0
  no shutdown
!
interface GigabitEthernet0/3
  description New Interface
  ip address 172.16.0.1 255.255.255.0
  no shutdown
!
end"#;

        let (_additions, _deletions, _modifications, summary, unified) =
            compute_diff(config1, config2);

        // Should detect:
        // 1. Description change on Gi0/1
        // 2. IP address change on Gi0/1
        // 3. New interface Gi0/3 (multiple lines added)
        assert!(summary.lines_added > 0, "Should detect additions");
        assert!(
            unified.contains("Router-New") || unified.contains("172.16.0.1"),
            "Should show new content"
        );
        assert!(
            unified.contains('-') && unified.contains('+'),
            "Should have both deletions and additions"
        );
    }

    // -------------------------------------------------------------------------
    // LLM Format Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_diff_result_llm_format_identical() {
        let result = DiffResult {
            node: "SW-Core-01".to_string(),
            version1: "abc123".to_string(),
            version2: "def456".to_string(),
            identical: true,
            summary: DiffSummary::default(),
            additions: vec![],
            deletions: vec![],
            modifications: vec![],
            unified_diff: String::new(),
            redaction: RedactionMetadata::default(),
        };

        let output = result.to_llm_format();

        assert!(output.contains("## Configuration Diff: SW-Core-01"));
        assert!(output.contains("abc123"));
        assert!(output.contains("def456"));
        assert!(output.contains("Configurations are identical"));
    }

    #[test]
    fn test_diff_result_llm_format_with_changes() {
        let result = DiffResult {
            node: "SW-Core-01".to_string(),
            version1: "abc123".to_string(),
            version2: "def456".to_string(),
            identical: false,
            summary: DiffSummary {
                lines_added: 2,
                lines_removed: 1,
                modification_blocks: 1,
            },
            additions: vec![LineChange {
                line_num: 5,
                content: "new line".to_string(),
            }],
            deletions: vec![LineChange {
                line_num: 3,
                content: "removed line".to_string(),
            }],
            modifications: vec![Modification {
                v1_line_start: 2,
                v1_line_end: 2,
                v2_line_start: 2,
                v2_line_end: 2,
                old_content: vec!["old value".to_string()],
                new_content: vec!["new value".to_string()],
            }],
            unified_diff: "@@ -1,3 +1,4 @@\n line1\n-old value\n+new value\n line3\n+new line"
                .to_string(),
            redaction: RedactionMetadata::default(),
        };

        let output = result.to_llm_format();

        // Verify structure
        assert!(output.contains("### Summary"));
        assert!(output.contains("- Lines added: 2"));
        assert!(output.contains("- Lines removed: 1"));
        assert!(output.contains("- Modification blocks: 1"));

        // Verify unified diff section
        assert!(output.contains("### Unified Diff"));
        assert!(output.contains("```diff"));

        // Verify pure additions section
        assert!(output.contains("### Pure Additions"));
        assert!(output.contains("+ [line 5] new line"));

        // Verify pure deletions section
        assert!(output.contains("### Pure Deletions"));
        assert!(output.contains("- [line 3] removed line"));
    }

    #[test]
    fn test_diff_result_llm_format_unified_only() {
        let result = DiffResult {
            node: "SW-01".to_string(),
            version1: "v1".to_string(),
            version2: "v2".to_string(),
            identical: false,
            summary: DiffSummary {
                lines_added: 1,
                lines_removed: 1,
                modification_blocks: 1,
            },
            additions: vec![], // No pure additions
            deletions: vec![], // No pure deletions
            modifications: vec![Modification {
                v1_line_start: 1,
                v1_line_end: 1,
                v2_line_start: 1,
                v2_line_end: 1,
                old_content: vec!["old".to_string()],
                new_content: vec!["new".to_string()],
            }],
            unified_diff: "@@ -1 +1 @@\n-old\n+new".to_string(),
            redaction: RedactionMetadata::default(),
        };

        let output = result.to_llm_format();

        assert!(output.contains("### Unified Diff"));
        assert!(!output.contains("### Pure Additions"));
        assert!(!output.contains("### Pure Deletions"));
    }

    // -------------------------------------------------------------------------
    // DiffResult Serialization Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_diff_result_serializes() {
        let result = DiffResult {
            node: "SW-01".to_string(),
            version1: "v1".to_string(),
            version2: "v2".to_string(),
            identical: false,
            summary: DiffSummary {
                lines_added: 1,
                lines_removed: 0,
                modification_blocks: 0,
            },
            additions: vec![LineChange {
                line_num: 1,
                content: "test".to_string(),
            }],
            deletions: vec![],
            modifications: vec![],
            unified_diff: "+test".to_string(),
            redaction: RedactionMetadata::default(),
        };

        let json = serde_json::to_string(&result).expect("Should serialize");
        assert!(json.contains("\"node\":\"SW-01\""));
        assert!(json.contains("\"identical\":false"));
        assert!(json.contains("\"lines_added\":1"));
        assert!(json.contains("\"unified_diff\""));
    }

    #[test]
    fn test_line_change_serializes() {
        let change = LineChange {
            line_num: 42,
            content: "test content".to_string(),
        };

        let json = serde_json::to_string(&change).expect("Should serialize");
        assert!(json.contains("\"line_num\":42"));
        assert!(json.contains("\"content\":\"test content\""));
    }

    #[test]
    fn test_modification_serializes() {
        let modification = Modification {
            v1_line_start: 10,
            v1_line_end: 12,
            v2_line_start: 10,
            v2_line_end: 11,
            old_content: vec!["old1".to_string(), "old2".to_string(), "old3".to_string()],
            new_content: vec!["new1".to_string(), "new2".to_string()],
        };

        let json = serde_json::to_string(&modification).expect("Should serialize");
        assert!(json.contains("\"v1_line_start\":10"));
        assert!(json.contains("\"v1_line_end\":12"));
        assert!(json.contains("\"old_content\""));
        assert!(json.contains("\"new_content\""));
    }

    // -------------------------------------------------------------------------
    // Summary Statistics Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_diff_summary_default() {
        let summary = DiffSummary::default();
        assert_eq!(summary.lines_added, 0);
        assert_eq!(summary.lines_removed, 0);
        assert_eq!(summary.modification_blocks, 0);
    }

    #[test]
    fn test_diff_summary_equality() {
        let summary1 = DiffSummary {
            lines_added: 1,
            lines_removed: 2,
            modification_blocks: 3,
        };
        let summary2 = DiffSummary {
            lines_added: 1,
            lines_removed: 2,
            modification_blocks: 3,
        };
        assert_eq!(summary1, summary2);
    }

    // -------------------------------------------------------------------------
    // Edge Case Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_compute_diff_whitespace_only_changes() {
        let config1 = "line1\nline2\nline3";
        let config2 = "line1\nline2 \nline3"; // Added trailing space

        let (_additions, _deletions, _modifications, summary, _unified) =
            compute_diff(config1, config2);

        // Should detect the whitespace change
        assert!(
            summary.lines_added > 0 || summary.lines_removed > 0,
            "Should detect whitespace change"
        );
    }

    #[test]
    fn test_compute_diff_large_config() {
        // Test with a larger config to ensure performance
        let mut config1 = String::new();
        let mut config2 = String::new();

        for i in 0..1000 {
            config1.push_str(&format!("line {}\n", i));
            config2.push_str(&format!("line {}\n", i));
        }

        // Modify a few lines in config2
        config2 = config2.replace("line 500", "modified line 500");
        config2 = config2.replace("line 750", "modified line 750");
        config2.push_str("extra line 1000\n");

        let (_additions, _deletions, _modifications, summary, unified) =
            compute_diff(&config1, &config2);

        assert!(summary.lines_added >= 1, "Should detect additions");
        assert!(
            summary.lines_removed >= 2,
            "Should detect the modifications"
        );
        assert!(
            !unified.is_empty(),
            "Should generate unified diff for large files"
        );
    }
}
