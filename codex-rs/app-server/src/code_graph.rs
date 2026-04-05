use codex_app_server_protocol::CodeGraphCacheStatus;
use codex_app_server_protocol::CodeGraphFileSlice;
use codex_app_server_protocol::CodeGraphNode;
use codex_app_server_protocol::CodeGraphNodeImportance;
use codex_app_server_protocol::CodeGraphNodeType;
use codex_app_server_protocol::CodeGraphQueryResponse;
use codex_code_graph::ExecutionGraphService;
use codex_code_graph::GraphCacheStatus as CoreGraphCacheStatus;
use codex_code_graph::GraphNodeImportance as CoreGraphNodeImportance;
use codex_code_graph::GraphNodeType as CoreGraphNodeType;
use codex_code_graph::GraphQueryOptions;
use std::path::PathBuf;

pub(crate) fn run_code_graph_query(
    codex_home: PathBuf,
    query: String,
    roots: Vec<String>,
    limit: Option<u32>,
    expand_nodes: Option<Vec<String>>,
) -> anyhow::Result<CodeGraphQueryResponse> {
    let mut service = ExecutionGraphService::new(codex_home);
    let result = service.query_graph(
        query.as_str(),
        &roots,
        GraphQueryOptions {
            max_nodes: limit.unwrap_or(8) as usize,
            expand_nodes: expand_nodes.unwrap_or_default(),
        },
    )?;

    Ok(CodeGraphQueryResponse {
        entry_node_ids: result.entry_node_ids,
        flow: result.flow,
        nodes: result
            .nodes
            .into_iter()
            .map(|node| CodeGraphNode {
                id: node.id,
                node_type: match node.node_type {
                    CoreGraphNodeType::Function => CodeGraphNodeType::Function,
                    CoreGraphNodeType::Component => CodeGraphNodeType::Component,
                    CoreGraphNodeType::Hook => CodeGraphNodeType::Hook,
                    CoreGraphNodeType::Api => CodeGraphNodeType::Api,
                    CoreGraphNodeType::Event => CodeGraphNodeType::Event,
                    CoreGraphNodeType::Module => CodeGraphNodeType::Module,
                },
                file: node.file,
                what_it_is: node.what_it_is,
                behavior_summary: node.behavior_summary,
                calls: node.calls,
                triggered_by: node.triggered_by,
                affects: node.affects,
                side_effects: node.side_effects,
                importance: match node.importance {
                    CoreGraphNodeImportance::Low => CodeGraphNodeImportance::Low,
                    CoreGraphNodeImportance::Medium => CodeGraphNodeImportance::Medium,
                    CoreGraphNodeImportance::High => CodeGraphNodeImportance::High,
                },
            })
            .collect(),
        files: result
            .files
            .into_iter()
            .map(|file| CodeGraphFileSlice {
                node_id: file.node_id,
                file: file.file,
                start_line: file.start_line,
                end_line: file.end_line,
                content: file.content,
            })
            .collect(),
        compiled_context: result.compiled_context,
        cache_status: match result.cache_status {
            CoreGraphCacheStatus::Hit => CodeGraphCacheStatus::Hit,
            CoreGraphCacheStatus::Miss => CodeGraphCacheStatus::Miss,
            CoreGraphCacheStatus::Refreshed => CodeGraphCacheStatus::Refreshed,
        },
    })
}
