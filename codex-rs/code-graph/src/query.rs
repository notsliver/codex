use crate::builder::ExecutionGraph;
use crate::builder::flatten_nodes;
use crate::types::GraphFileMatch;
use crate::types::GraphFileSlice;
use crate::types::GraphNode;
use crate::types::GraphQueryOptions;
use crate::types::GraphQueryResult;
use nucleo::Config;
use nucleo::Matcher;
use nucleo::Utf32String;
use nucleo::pattern::AtomKind;
use nucleo::pattern::CaseMatching;
use nucleo::pattern::Normalization;
use nucleo::pattern::Pattern;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub(crate) fn query_graph(
    graph: &mut ExecutionGraph,
    query: &str,
    options: GraphQueryOptions,
) -> anyhow::Result<GraphQueryResult> {
    let nodes = flatten_nodes(graph);
    let mut entry_nodes = select_entry_nodes(&nodes, query, options.max_nodes);
    if entry_nodes.is_empty() {
        entry_nodes = select_file_entry_nodes(graph, query, options.max_nodes);
    }
    let expanded = collect_execution_flow(graph, &entry_nodes, &options.expand_nodes);
    let entry_node_ids = entry_nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let files = build_file_slices(graph, &expanded);
    let flow = expanded
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let compiled_context = compile_context(&entry_nodes, &expanded);

    Ok(GraphQueryResult {
        entry_node_ids,
        flow,
        nodes: expanded,
        files,
        compiled_context,
        cache_status: graph.cache_status,
    })
}

pub(crate) fn query_graph_file_matches(
    graph: &mut ExecutionGraph,
    query: &str,
) -> anyhow::Result<Vec<GraphFileMatch>> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut indices_matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut per_file = BTreeMap::<String, GraphFileMatch>::new();
    for file_entry in graph.files.values() {
        let Some(module_node) = file_entry
            .nodes
            .iter()
            .find(|record| record.node.node_type == crate::types::GraphNodeType::Module)
            .map(|record| &record.node)
        else {
            continue;
        };
        let haystack = Utf32String::from(file_entry.relative_path.as_str());
        let (score, indices) = if let Some(score) = pattern.score(haystack.slice(..), &mut matcher)
        {
            let mut indices = Vec::<u32>::new();
            let _ = pattern.indices(haystack.slice(..), &mut indices_matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            (score, Some(indices))
        } else {
            let query_terms = tokenize(query);
            let semantic_haystack = file_entry
                .nodes
                .iter()
                .map(|record| {
                    format!(
                        "{} {} {}",
                        record.node.id, record.node.what_it_is, record.node.behavior_summary
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            if query_terms
                .iter()
                .all(|term| semantic_haystack.contains(term))
            {
                (1, None)
            } else {
                continue;
            }
        };
        let file_name = Path::new(&module_node.file)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(module_node.file.as_str())
            .to_string();
        per_file
            .entry(module_node.file.clone())
            .or_insert(GraphFileMatch {
                root: file_entry.root.clone(),
                path: module_node.file.clone(),
                file_name,
                score,
                indices,
            });
    }
    let mut matches = per_file.into_values().collect::<Vec<_>>();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    Ok(matches)
}

fn select_entry_nodes(nodes: &[GraphNode], query: &str, max_nodes: usize) -> Vec<GraphNode> {
    let ranked = rank_nodes(nodes, query);
    let max_nodes = max_nodes.max(1);
    ranked
        .into_iter()
        .take(max_nodes)
        .map(|(node, _score)| node.clone())
        .collect()
}

fn select_file_entry_nodes(graph: &mut ExecutionGraph, query: &str, max_nodes: usize) -> Vec<GraphNode> {
    let matches = query_graph_file_matches(graph, query).unwrap_or_default();
    let mut selected = Vec::new();
    for file_match in matches.into_iter().take(max_nodes.max(1)) {
        if let Some(file_entry) = graph.files.get(&file_match.path) {
            if let Some(module_node) = file_entry
                .nodes
                .iter()
                .find(|record| record.node.node_type == crate::types::GraphNodeType::Module)
            {
                selected.push(module_node.node.clone());
            }
        }
    }
    selected
}

fn rank_nodes<'a>(nodes: &'a [GraphNode], query: &str) -> Vec<(&'a GraphNode, i64)> {
    let query_terms = tokenize(query);
    let mut scored = nodes
        .iter()
        .filter_map(|node| {
            let haystack = format!(
                "{} {} {} {}",
                node.id, node.file, node.what_it_is, node.behavior_summary
            )
            .to_ascii_lowercase();
            let mut score = 0_i64;
            for term in &query_terms {
                if node.id.to_ascii_lowercase().contains(term) {
                    score += 20;
                }
                if node.file.to_ascii_lowercase().contains(term) {
                    score += 8;
                }
                if haystack.contains(term) {
                    score += 10;
                }
            }
            if score == 0 {
                return None;
            }
            if node.importance == crate::types::GraphNodeImportance::High {
                score += 30;
            } else if node.importance == crate::types::GraphNodeImportance::Medium {
                score += 15;
            }
            score += node.calls.len() as i64 * 2;
            score += node.triggered_by.len() as i64 * 3;
            Some((node, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(node, score)| (Reverse(*score), node.id.as_str()));
    scored
}

fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase())
        .collect()
}

fn collect_execution_flow(
    graph: &ExecutionGraph,
    entry_nodes: &[GraphNode],
    expand_nodes: &[String],
) -> Vec<GraphNode> {
    let all_nodes = flatten_nodes(graph)
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let expandable = expand_nodes.iter().cloned().collect::<HashSet<_>>();
    let mut visited = HashSet::new();
    let mut ordered = Vec::new();

    for entry in entry_nodes {
        let should_expand = expandable.contains(&entry.id) || entry_nodes.len() == 1;
        append_node(
            entry,
            &all_nodes,
            &expandable,
            should_expand,
            &mut visited,
            &mut ordered,
        );
    }
    ordered
}

fn append_node(
    node: &GraphNode,
    all_nodes: &BTreeMap<String, GraphNode>,
    expandable: &HashSet<String>,
    should_expand: bool,
    visited: &mut HashSet<String>,
    ordered: &mut Vec<GraphNode>,
) {
    if !visited.insert(node.id.clone()) {
        return;
    }
    ordered.push(node.clone());

    let child_expand = should_expand || expandable.contains(&node.id);
    if child_expand {
        for call in &node.calls {
            if let Some(next) = all_nodes.get(call) {
                append_node(next, all_nodes, expandable, child_expand, visited, ordered);
            }
        }
    }
}

fn build_file_slices(graph: &ExecutionGraph, expanded: &[GraphNode]) -> Vec<GraphFileSlice> {
    let mut slices = Vec::new();
    for node in expanded {
        let Some(file) = graph.files.get(&node.file) else {
            continue;
        };
        let Some(record) = file.nodes.iter().find(|record| record.node.id == node.id) else {
            continue;
        };
        let Some(span) = &record.span else {
            continue;
        };
        let absolute_path = Path::new(&file.root).join(&file.relative_path);
        let Ok(content) = fs::read_to_string(&absolute_path) else {
            continue;
        };
        let (slice, slice_start, slice_end) =
            extract_lines(&content, span.start_line, span.end_line);
        slices.push(GraphFileSlice {
            node_id: node.id.clone(),
            file: node.file.clone(),
            start_line: slice_start,
            end_line: slice_end,
            content: slice,
        });
    }
    slices
}

fn extract_lines(content: &str, start_line: usize, end_line: usize) -> (String, usize, usize) {
    let lines = content.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    let slice_start = start_line.saturating_sub(3).max(1);
    let slice_end = (end_line + 2).min(total_lines);
    let slice = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let line_number = index + 1;
            (slice_start..=slice_end)
                .contains(&line_number)
                .then_some(*line)
        })
        .collect::<Vec<_>>()
        .join("\n");
    (slice, slice_start, slice_end)
}

fn compile_context(entry_nodes: &[GraphNode], nodes: &[GraphNode]) -> String {
    let entry_ids = entry_nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    let entry_label = if entry_ids.len() == 1 {
        "entry point"
    } else {
        "entry points"
    };
    let entry = if entry_ids.is_empty() {
        "<none>".to_string()
    } else {
        entry_ids.join(", ")
    };
    let flow = if nodes.is_empty() {
        "<none>".to_string()
    } else {
        nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>()
            .join(" -> ")
    };
    let node_lines = nodes
        .iter()
        .map(|node| format!("- {}: {}", node.id, node.behavior_summary))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "TASK CONTEXT:\n- {entry_label}: {entry}\n- execution flow: {flow}\n\nNODES:\n{node_lines}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GraphNodeImportance, GraphNodeType};

    fn make_node(id: &str, score_text: &str) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            node_type: GraphNodeType::Function,
            file: "file.rs".to_string(),
            what_it_is: id.to_string(),
            behavior_summary: score_text.to_string(),
            calls: Vec::new(),
            triggered_by: Vec::new(),
            affects: Vec::new(),
            side_effects: Vec::new(),
            importance: GraphNodeImportance::Low,
        }
    }

    #[test]
    fn select_entry_nodes_returns_top_max_nodes() {
        let nodes = vec![
            make_node("crate::a", "search a"),
            make_node("crate::b", "search b"),
            make_node("crate::c", "search c"),
        ];
        let selected = select_entry_nodes(&nodes, "search", 2);
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn extract_lines_includes_surrounding_context() {
        let content = "line1\nline2\nline3\nline4\nline5\nline6";
        let (slice, start, end) = extract_lines(content, 3, 3);
        assert_eq!(start, 1);
        assert_eq!(end, 5);
        assert!(slice.contains("line1"));
        assert!(slice.contains("line5"));
    }
}
