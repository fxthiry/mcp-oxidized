//! Deterministic, paginated configuration search.

use regex::{Regex, RegexBuilder};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::instrument;

use crate::error::OxidizedError;
use crate::oxidized::{CacheMetadata, ConfSearchResult, Node, OxidizedBackend, OxidizedClient};
use crate::redaction::{RedactionMetadata, redact};

const MAX_CONCURRENT_REQUESTS: usize = 10;
pub const MAX_CONTEXT_LINES: usize = 50;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub nodes: Option<Vec<String>>,
    pub case_sensitive: bool,
    pub literal: bool,
    pub context_before: usize,
    pub context_after: usize,
    pub limit: usize,
    pub limit_per_node: Option<usize>,
    pub offset: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            nodes: None,
            case_sensitive: true,
            literal: false,
            context_before: 1,
            context_after: 1,
            limit: 100,
            limit_per_node: None,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchMatch {
    pub node: String,
    pub line_num: usize,
    /// Number of adjacent matching lines represented by this block.
    pub line_count: usize,
    pub content: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeMatches {
    pub node: String,
    /// Number of matching lines, including merged adjacent lines.
    pub match_count: usize,
    pub model: String,
    pub backup_timestamp: Option<String>,
    pub cache_status: CacheMetadata,
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub pattern: String,
    pub case_sensitive: bool,
    pub literal: bool,
    pub context_before: usize,
    pub context_after: usize,
    pub total_matches: usize,
    pub shown_matches: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
    pub nodes_searched: usize,
    pub configs_fetched: usize,
    pub nodes_with_matches: usize,
    pub nodes_returned: usize,
    pub results: Vec<NodeMatches>,
    pub warnings: Vec<String>,
    pub redaction: RedactionMetadata,
}

impl SearchResult {
    pub fn to_llm_format(&self) -> String {
        let mut output = format!(
            "## Configuration Search Results\n\n**Pattern:** `{}`\n\
             **Nodes searched:** {} | **Configs fetched:** {} | **Nodes with matches:** {}\n\
             **Showing:** {} matching lines from offset {} ({} total)\n",
            self.pattern,
            self.nodes_searched,
            self.configs_fetched,
            self.nodes_with_matches,
            self.shown_matches,
            self.offset,
            self.total_matches
        );
        for warning in &self.warnings {
            output.push_str(&format!("\n- Warning: {warning}"));
        }
        if self.results.is_empty() {
            output.push_str("\n\nNo matches found.\n");
            return output;
        }
        for node in &self.results {
            output.push_str(&format!(
                "\n\n### {} ({} matching lines)\n",
                node.node, node.match_count
            ));
            for matched in &node.matches {
                output.push_str(&format!("\n**Line {}:**\n```\n", matched.line_num));
                for line in &matched.context_before {
                    output.push_str(&format!("  {line}\n"));
                }
                for line in matched.content.lines() {
                    output.push_str(&format!("> {line}\n"));
                }
                for line in &matched.context_after {
                    output.push_str(&format!("  {line}\n"));
                }
                output.push_str("```\n");
            }
        }
        output
    }
}

/// Compatibility wrapper retaining the v1 call signature.
pub async fn search_configs(
    backend: &OxidizedClient,
    pattern: &str,
    nodes: Option<Vec<String>>,
    case_sensitive: bool,
    limit: u32,
) -> Result<SearchResult, OxidizedError> {
    search_configs_with_options(
        backend,
        pattern,
        SearchOptions {
            nodes,
            case_sensitive,
            limit: limit as usize,
            ..SearchOptions::default()
        },
    )
    .await
}

#[instrument(skip(backend, options), fields(pattern = %pattern))]
pub async fn search_configs_with_options(
    backend: &OxidizedClient,
    pattern: &str,
    options: SearchOptions,
) -> Result<SearchResult, OxidizedError> {
    if pattern.is_empty() {
        return Err(OxidizedError::InvalidRegex(
            "Empty pattern is not allowed".to_string(),
        ));
    }
    if options.context_before > MAX_CONTEXT_LINES || options.context_after > MAX_CONTEXT_LINES {
        return Err(OxidizedError::InvalidRegex(format!(
            "Search context is limited to {MAX_CONTEXT_LINES} lines before and after"
        )));
    }
    if !(1..=1000).contains(&options.limit)
        || options
            .limit_per_node
            .is_some_and(|limit| !(1..=1000).contains(&limit))
    {
        return Err(OxidizedError::InvalidRegex(
            "Search limits must be between 1 and 1000".to_string(),
        ));
    }

    let effective_pattern = if options.literal {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let regex = RegexBuilder::new(&effective_pattern)
        .case_insensitive(!options.case_sensitive)
        .build()
        .map_err(|error| {
            OxidizedError::InvalidRegex(format!("Invalid regex pattern '{pattern}': {error}"))
        })?;

    let (mut all_nodes, _) = backend.get_nodes().await?;
    all_nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let by_name: HashMap<String, Node> = all_nodes
        .into_iter()
        .map(|node| (node.name.clone(), node))
        .collect();
    let mut warnings = Vec::new();

    let mut scoped: Vec<String> = match options.nodes.clone() {
        Some(nodes) => nodes
            .into_iter()
            .filter(|name| {
                if by_name.contains_key(name) {
                    true
                } else {
                    warnings.push(format!("Node '{name}' not found, skipping"));
                    false
                }
            })
            .collect(),
        None => by_name.keys().cloned().collect(),
    };
    scoped.sort();
    scoped.dedup();
    let nodes_searched = scoped.len();

    // literal mode cannot safely use the server endpoint because that endpoint
    // does not expose matching semantics. Empty Available is a real zero-match
    // result; only Unavailable falls back to all scoped nodes.
    let candidates = if options.literal {
        scoped.clone()
    } else {
        match backend.conf_search(pattern).await? {
            ConfSearchResult::Available(nodes) => {
                let available: HashSet<_> = nodes.into_iter().collect();
                scoped
                    .iter()
                    .filter(|name| available.contains(*name))
                    .cloned()
                    .collect()
            }
            ConfSearchResult::Unavailable(reason) => {
                warnings.push(format!(
                    "Oxidized configuration prefilter unavailable ({reason}); used full search"
                ));
                scoped.clone()
            }
        }
    };

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let mut tasks = JoinSet::new();
    for name in candidates {
        let client = backend.clone();
        let semaphore = Arc::clone(&semaphore);
        let node = by_name.get(&name).cloned().expect("candidate is scoped");
        tasks.spawn(async move {
            let _permit =
                semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| OxidizedError::HttpError {
                        status_code: 503,
                        context: "search semaphore closed".to_string(),
                    })?;
            let config = client.get_node_config(&name).await;
            Ok::<_, OxidizedError>((node, config))
        });
    }

    let mut configs_fetched = 0usize;
    let mut matches_by_node = Vec::new();
    while let Some(task) = tasks.join_next().await {
        match task {
            Ok(Ok((node, Ok((config, cache_status))))) => {
                configs_fetched += 1;
                let mut matches = search_in_config(
                    &node.name,
                    &config,
                    &regex,
                    options.context_before,
                    options.context_after,
                );
                if let Some(limit) = options.limit_per_node {
                    truncate_matching_lines(&mut matches, limit);
                }
                if !matches.is_empty() {
                    let match_count = matching_line_count(&matches);
                    matches_by_node.push(NodeMatches {
                        node: node.name,
                        match_count,
                        model: node.model,
                        backup_timestamp: node
                            .mtime
                            .or_else(|| node.last.as_ref().and_then(|last| last.end.clone())),
                        cache_status,
                        matches,
                    });
                }
            }
            Ok(Ok((node, Err(error)))) => {
                warnings.push(format!(
                    "Error fetching config for '{}': {error}",
                    node.name
                ));
            }
            Ok(Err(error)) => warnings.push(format!("Search task error: {error}")),
            Err(error) => warnings.push(format!("Search task join error: {error}")),
        }
    }
    matches_by_node.sort_by(|a, b| a.node.cmp(&b.node));

    let nodes_with_matches = matches_by_node.len();
    let total_matches: usize = matches_by_node.iter().map(|node| node.match_count).sum();
    let mut skip = options.offset;
    let mut take = options.limit;
    let mut returned = Vec::new();
    let mut replacements = 0usize;

    for mut node in matches_by_node {
        let mut selected = Vec::new();
        for mut block in node.matches {
            if skip >= block.line_count {
                skip -= block.line_count;
                continue;
            }
            if skip > 0 {
                trim_block_front(&mut block, skip);
                skip = 0;
            }
            if take == 0 {
                break;
            }
            if block.line_count > take {
                trim_block_back(&mut block, take);
            }
            take -= block.line_count;
            selected.push(block);
        }
        if !selected.is_empty() {
            replacements += redact_matches(&mut selected, backend.redaction_enabled());
            node.matches = selected;
            node.match_count = matching_line_count(&node.matches);
            returned.push(node);
        }
        if take == 0 {
            break;
        }
    }

    let shown_matches: usize = returned.iter().map(|node| node.match_count).sum();
    let nodes_returned = returned.len();
    Ok(SearchResult {
        pattern: pattern.to_string(),
        case_sensitive: options.case_sensitive,
        literal: options.literal,
        context_before: options.context_before,
        context_after: options.context_after,
        total_matches,
        shown_matches,
        offset: options.offset,
        limit: options.limit,
        has_more: options.offset.saturating_add(shown_matches) < total_matches,
        nodes_searched,
        configs_fetched,
        nodes_with_matches,
        nodes_returned,
        results: returned,
        warnings,
        redaction: RedactionMetadata {
            enabled: backend.redaction_enabled(),
            replacement_count: replacements,
        },
    })
}

fn matching_line_count(matches: &[SearchMatch]) -> usize {
    matches.iter().map(|matched| matched.line_count).sum()
}

fn truncate_matching_lines(matches: &mut Vec<SearchMatch>, limit: usize) {
    let mut remaining = limit;
    matches.retain_mut(|block| {
        if remaining == 0 {
            return false;
        }
        if block.line_count > remaining {
            trim_block_back(block, remaining);
        }
        remaining -= block.line_count;
        true
    });
}

fn trim_block_front(block: &mut SearchMatch, count: usize) {
    block.line_num += count;
    block.line_count -= count;
    block.content = block
        .content
        .lines()
        .skip(count)
        .collect::<Vec<_>>()
        .join("\n");
}

fn trim_block_back(block: &mut SearchMatch, keep: usize) {
    block.line_count = keep;
    block.content = block
        .content
        .lines()
        .take(keep)
        .collect::<Vec<_>>()
        .join("\n");
}

fn redact_matches(matches: &mut [SearchMatch], enabled: bool) -> usize {
    let mut replacements = 0;
    for matched in matches {
        let result = redact(&matched.content, enabled);
        matched.content = result.text;
        replacements += result.redaction.replacement_count;
        for line in matched
            .context_before
            .iter_mut()
            .chain(matched.context_after.iter_mut())
        {
            let result = redact(line, enabled);
            *line = result.text;
            replacements += result.redaction.replacement_count;
        }
    }
    replacements
}

/// Return merged blocks for adjacent matching lines so context is not repeated.
fn search_in_config(
    node: &str,
    config: &str,
    regex: &Regex,
    context_before: usize,
    context_after: usize,
) -> Vec<SearchMatch> {
    let lines: Vec<&str> = config.lines().collect();
    let matched: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| regex.is_match(line).then_some(index))
        .collect();
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while cursor < matched.len() {
        let start = matched[cursor];
        let mut end = start;
        while cursor + 1 < matched.len() && matched[cursor + 1] == end + 1 {
            cursor += 1;
            end = matched[cursor];
        }
        blocks.push(SearchMatch {
            node: node.to_string(),
            line_num: start + 1,
            line_count: end - start + 1,
            content: lines[start..=end].join("\n"),
            context_before: lines[start.saturating_sub(context_before)..start]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            context_after: lines[end + 1..(end + 1 + context_after).min(lines.len())]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        });
        cursor += 1;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_mode_escapes_regex_characters() {
        let regex = Regex::new(&regex::escape("10.0.[1]")).unwrap();
        assert!(regex.is_match("address 10.0.[1]"));
        assert!(!regex.is_match("address 10x0x1"));
    }

    #[test]
    fn adjacent_matches_are_merged_with_configurable_context() {
        let regex = Regex::new("match").unwrap();
        let matches = search_in_config(
            "r1",
            "before2\nbefore1\nmatch one\nmatch two\nafter1\nafter2",
            &regex,
            2,
            2,
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_count, 2);
        assert_eq!(matches[0].context_before, ["before2", "before1"]);
        assert_eq!(matches[0].context_after, ["after1", "after2"]);
    }

    #[test]
    fn per_node_limit_can_trim_merged_block() {
        let regex = Regex::new("match").unwrap();
        let mut matches = search_in_config("r1", "match1\nmatch2\nmatch3", &regex, 0, 0);
        truncate_matching_lines(&mut matches, 2);
        assert_eq!(matching_line_count(&matches), 2);
        assert_eq!(matches[0].content, "match1\nmatch2");
    }

    #[test]
    fn redacts_matches_and_context_after_searching_raw_text() {
        let regex = Regex::new("public").unwrap();
        let mut matches = search_in_config(
            "r1",
            "hostname r1\nsnmp-server community public ro",
            &regex,
            1,
            0,
        );
        let count = redact_matches(&mut matches, true);
        assert_eq!(count, 1);
        assert!(matches[0].content.contains("<redacted>"));
        assert!(!matches[0].content.contains("public"));
    }
}
