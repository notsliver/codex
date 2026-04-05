use crate::cache_root;
use crate::parser::parse_workspace;
use crate::types::GraphCacheStatus;
use crate::types::GraphNode;
use crate::types::GraphNodeImportance;
use crate::types::NodeRecord;
use anyhow::Context;
use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGraph {
    pub cache_status: GraphCacheStatus,
    pub root_signature: String,
    pub files: BTreeMap<String, FileGraphEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGraphEntry {
    pub root: String,
    pub relative_path: String,
    pub file_hash: String,
    pub nodes: Vec<NodeRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedGraph {
    root_signature: String,
    files: Vec<PersistedFileGraphEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedFileGraphEntry {
    root: String,
    relative_path: String,
    file_hash: String,
    nodes: Vec<NodeRecord>,
}

pub fn load_or_update_graph(
    codex_home: &Path,
    roots: &[String],
) -> anyhow::Result<ExecutionGraph> {
    let root_signature = root_signature(roots);
    let persisted = load_persisted_graph(codex_home, &root_signature)?;
    let previous_files = persisted
        .as_ref()
        .map(|graph| {
            graph
                .files
                .iter()
                .map(|entry| {
                    (
                        entry.relative_path.clone(),
                        FileGraphEntry {
                            root: entry.root.clone(),
                            relative_path: entry.relative_path.clone(),
                            file_hash: entry.file_hash.clone(),
                            nodes: entry.nodes.clone(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let parsed_files = parse_workspace(roots, &previous_files)?;
    let cache_status = match persisted {
        Some(ref graph) if graph.root_signature == root_signature => {
            if parsed_files == previous_files {
                GraphCacheStatus::Hit
            } else {
                GraphCacheStatus::Refreshed
            }
        }
        Some(_) => GraphCacheStatus::Refreshed,
        None => GraphCacheStatus::Miss,
    };

    let mut graph = ExecutionGraph {
        cache_status,
        root_signature,
        files: parsed_files,
    };
    populate_graph_metadata(&mut graph);
    Ok(graph)
}

fn populate_graph_metadata(graph: &mut ExecutionGraph) {
    let mut called_by = BTreeMap::<String, Vec<String>>::new();

    for file in graph.files.values() {
        for record in &file.nodes {
            for target in &record.node.calls {
                called_by
                    .entry(target.clone())
                    .or_default()
                    .push(record.node.id.clone());
            }
        }
    }

    for callers in called_by.values_mut() {
        callers.sort();
        callers.dedup();
    }

    for file in graph.files.values_mut() {
        for record in &mut file.nodes {
            record.node.triggered_by = called_by.get(&record.node.id).cloned().unwrap_or_default();
            record.node.importance = importance_for_node(&record.node);
        }
    }
}

fn importance_for_node(node: &GraphNode) -> GraphNodeImportance {
    if node.importance == GraphNodeImportance::High || node.triggered_by.len() >= 4 {
        GraphNodeImportance::High
    } else if node.importance == GraphNodeImportance::Medium || !node.triggered_by.is_empty() {
        GraphNodeImportance::Medium
    } else {
        GraphNodeImportance::Low
    }
}

pub(crate) fn persist_graph(
    codex_home: &Path,
    roots: &[String],
    graph: &ExecutionGraph,
) -> anyhow::Result<()> {
    let directory = cache_root(codex_home);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create code graph cache directory at {}",
            directory.display()
        )
    })?;

    let path = graph_path(codex_home, roots);
    let persisted = PersistedGraph {
        root_signature: graph.root_signature.clone(),
        files: graph
            .files
            .values()
            .cloned()
            .map(|entry| PersistedFileGraphEntry {
                root: entry.root,
                relative_path: entry.relative_path,
                file_hash: entry.file_hash,
                nodes: entry.nodes,
            })
            .collect(),
    };
    let json =
        serde_json::to_vec_pretty(&persisted).context("failed to serialize code graph cache")?;
    fs::write(&path, json)
        .with_context(|| format!("failed to write code graph cache to {}", path.display()))?;
    Ok(())
}

fn load_persisted_graph(
    codex_home: &Path,
    root_signature: &str,
) -> anyhow::Result<Option<PersistedGraph>> {
    let path = cache_root(codex_home).join(format!("{root_signature}.json"));
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read code graph cache {}", path.display()))?;
    let graph = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to deserialize code graph cache {}", path.display()))?;
    Ok(Some(graph))
}

fn graph_path(codex_home: &Path, roots: &[String]) -> PathBuf {
    cache_root(codex_home).join(format!("{}.json", root_signature(roots)))
}

pub(crate) fn root_signature(roots: &[String]) -> String {
    let mut digest = Sha1::new();
    for root in roots {
        digest.update(root.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

pub(crate) fn flatten_nodes(graph: &ExecutionGraph) -> Vec<GraphNode> {
    let mut nodes = Vec::new();
    for file in graph.files.values() {
        for record in &file.nodes {
            nodes.push(record.node.clone());
        }
    }
    nodes
}
