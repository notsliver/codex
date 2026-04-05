use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GraphNodeType {
    Function,
    Component,
    Hook,
    Api,
    Event,
    Module,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum GraphNodeImportance {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: GraphNodeType,
    pub file: String,
    pub what_it_is: String,
    pub behavior_summary: String,
    pub calls: Vec<String>,
    pub triggered_by: Vec<String>,
    pub affects: Vec<String>,
    pub side_effects: Vec<String>,
    pub importance: GraphNodeImportance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphFileSlice {
    pub node_id: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphFileMatch {
    pub root: String,
    pub path: String,
    pub file_name: String,
    pub score: u32,
    pub indices: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GraphCacheStatus {
    Hit,
    Miss,
    Refreshed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphQueryOptions {
    pub max_nodes: usize,
    pub expand_nodes: Vec<String>,
}

impl Default for GraphQueryOptions {
    fn default() -> Self {
        Self {
            max_nodes: 8,
            expand_nodes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphQueryResult {
    pub entry_node_ids: Vec<String>,
    pub flow: Vec<String>,
    pub nodes: Vec<GraphNode>,
    pub files: Vec<GraphFileSlice>,
    pub compiled_context: String,
    pub cache_status: GraphCacheStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineSpan {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeRecord {
    pub node: GraphNode,
    pub local_calls: Vec<String>,
    pub imports: Vec<String>,
    pub span: Option<LineSpan>,
}
