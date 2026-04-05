mod builder;
mod parser;
mod query;
mod types;

pub use types::GraphCacheStatus;
pub use types::GraphFileMatch;
pub use types::GraphFileSlice;
pub use types::GraphNode;
pub use types::GraphNodeImportance;
pub use types::GraphNodeType;
pub use types::GraphQueryOptions;
pub use types::GraphQueryResult;
pub use types::NodeRecord;

pub use builder::load_or_update_graph;
pub use builder::ExecutionGraph;
pub use builder::FileGraphEntry;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

pub struct ExecutionGraphService {
    codex_home: PathBuf,
    query_cache: HashMap<QueryCacheKey, GraphQueryResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueryCacheKey {
    query: String,
    roots: Vec<String>,
    max_nodes: usize,
    expand_nodes: Vec<String>,
}

impl ExecutionGraphService {
    pub fn new(codex_home: PathBuf) -> Self {
        Self {
            codex_home,
            query_cache: HashMap::new(),
        }
    }

    pub fn query_graph(
        &mut self,
        query: &str,
        roots: &[String],
        options: GraphQueryOptions,
    ) -> anyhow::Result<GraphQueryResult> {
        let mut graph = builder::load_or_update_graph(self.codex_home.as_path(), roots)?;
        if graph.cache_status != GraphCacheStatus::Hit {
            self.query_cache.clear();
        }

        let key = QueryCacheKey {
            query: query.to_string(),
            roots: roots.to_owned(),
            max_nodes: options.max_nodes,
            expand_nodes: options.expand_nodes.clone(),
        };
        if let Some(cached) = self.query_cache.get(&key) {
            return Ok(cached.clone());
        }

        let result = query::query_graph(&mut graph, query, options)?;
        self.query_cache.insert(key, result.clone());
        builder::persist_graph(self.codex_home.as_path(), roots, &graph)?;
        Ok(result)
    }
}

pub async fn query_graph_file_matches(
    codex_home: &Path,
    roots: &[String],
    query: &str,
) -> anyhow::Result<Vec<GraphFileMatch>> {
    let mut graph = builder::load_or_update_graph(codex_home, roots)?;
    let matches = query::query_graph_file_matches(&mut graph, query)?;
    builder::persist_graph(codex_home, roots, &graph)?;
    Ok(matches)
}

pub fn cache_root(codex_home: &Path) -> PathBuf {
    codex_home.join("cache").join("code_graph")
}
