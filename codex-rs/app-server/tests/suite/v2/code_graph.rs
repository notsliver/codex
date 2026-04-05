use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::CodeGraphQueryParams;
use codex_app_server_protocol::CodeGraphQueryResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test]
async fn code_graph_query_returns_graph_first_context() -> Result<()> {
    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    std::fs::write(
        workspace.path().join("lib.rs"),
        r#"
pub fn query_graph() {
    build_graph();
}

fn build_graph() {
    persist_graph();
}

fn persist_graph() {}
"#,
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_code_graph_query_request(CodeGraphQueryParams {
            query: "query graph".to_string(),
            roots: vec![workspace.path().display().to_string()],
            limit: Some(4),
            expand_nodes: Some(vec!["crate::query_graph".to_string()]),
        })
        .await?;
    let response = mcp
        .read_stream_until_response_message(RequestId::Integer(request_id))
        .await?;
    let result: CodeGraphQueryResponse = to_response(response)?;

    assert_eq!(
        result.entry_node_ids,
        vec![
            "crate::query_graph".to_string(),
            "crate::build_graph".to_string(),
            "crate::persist_graph".to_string(),
            "module::lib.rs".to_string(),
        ]
    );
    assert_eq!(
        result.flow,
        vec![
            "crate::query_graph".to_string(),
            "crate::build_graph".to_string(),
            "crate::persist_graph".to_string(),
            "module::lib.rs".to_string(),
        ]
    );
    assert_eq!(result.files.len(), 3);
    assert_eq!(
        result
            .files
            .iter()
            .map(|file| file.node_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "crate::query_graph",
            "crate::build_graph",
            "crate::persist_graph",
        ]
    );

    let build_graph_node = result
        .nodes
        .iter()
        .find(|node| node.id == "crate::build_graph")
        .expect("build_graph node present");
    assert_eq!(
        build_graph_node.triggered_by,
        vec!["crate::query_graph".to_string()]
    );

    let persist_graph_node = result
        .nodes
        .iter()
        .find(|node| node.id == "crate::persist_graph")
        .expect("persist_graph node present");
    assert!(
        persist_graph_node
            .side_effects
            .contains(&"persist".to_string())
    );
    assert!(result.compiled_context.contains("TASK CONTEXT:"));
    Ok(())
}
