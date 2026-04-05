use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_app_server_protocol::FuzzyFileSearchMatchType;
use codex_app_server_protocol::FuzzyFileSearchResult;
use codex_app_server_protocol::FuzzyFileSearchSessionCompletedNotification;
use codex_app_server_protocol::FuzzyFileSearchSessionUpdatedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_code_graph::query_graph_file_matches;
use tracing::warn;

use crate::outgoing_message::OutgoingMessageSender;

const MATCH_LIMIT: usize = 50;

pub(crate) async fn run_fuzzy_file_search(
    codex_home: &Path,
    query: String,
    roots: Vec<String>,
    cancellation_flag: Arc<AtomicBool>,
) -> Vec<FuzzyFileSearchResult> {
    if cancellation_flag.load(Ordering::Relaxed) || roots.is_empty() {
        return Vec::new();
    }

    let files = match query_graph_file_matches(codex_home, &roots, query.as_str()).await {
        Ok(files) => files,
        Err(err) => {
            warn!("graph-backed fuzzy-file-search failed: {err}");
            Vec::new()
        }
    };
    if cancellation_flag.load(Ordering::Relaxed) {
        return Vec::new();
    }

    files
        .into_iter()
        .take(MATCH_LIMIT)
        .map(|file_match| FuzzyFileSearchResult {
            root: file_match.root,
            path: file_match.path,
            match_type: FuzzyFileSearchMatchType::File,
            file_name: file_match.file_name,
            score: file_match.score,
            indices: file_match.indices,
        })
        .collect()
}

pub(crate) struct FuzzyFileSearchSession {
    shared: Arc<SessionShared>,
}

impl FuzzyFileSearchSession {
    pub(crate) fn update_query(&self, query: String) {
        if self.shared.canceled.load(Ordering::Relaxed) {
            return;
        }
        {
            #[expect(clippy::unwrap_used)]
            let mut latest_query = self.shared.latest_query.lock().unwrap();
            *latest_query = query.clone();
        }

        let shared = Arc::clone(&self.shared);
        self.shared.runtime.spawn(async move {
            let files = if query.is_empty() {
                Vec::new()
            } else {
                match query_graph_file_matches(
                    shared.codex_home.as_path(),
                    &shared.roots,
                    query.as_str(),
                )
                .await
                {
                    Ok(files) => files
                        .into_iter()
                        .take(MATCH_LIMIT)
                        .map(|file_match| FuzzyFileSearchResult {
                            root: file_match.root,
                            path: file_match.path,
                            match_type: FuzzyFileSearchMatchType::File,
                            file_name: file_match.file_name,
                            score: file_match.score,
                            indices: file_match.indices,
                        })
                        .collect(),
                    Err(err) => {
                        warn!("graph-backed fuzzy-file-search session failed: {err}");
                        Vec::new()
                    }
                }
            };

            if shared.canceled.load(Ordering::Relaxed) {
                return;
            }

            let notification = ServerNotification::FuzzyFileSearchSessionUpdated(
                FuzzyFileSearchSessionUpdatedNotification {
                    session_id: shared.session_id.clone(),
                    query: query.clone(),
                    files,
                },
            );
            shared.outgoing.send_server_notification(notification).await;

            if shared.canceled.load(Ordering::Relaxed) {
                return;
            }

            let notification = ServerNotification::FuzzyFileSearchSessionCompleted(
                FuzzyFileSearchSessionCompletedNotification {
                    session_id: shared.session_id.clone(),
                },
            );
            shared.outgoing.send_server_notification(notification).await;
        });
    }
}

impl Drop for FuzzyFileSearchSession {
    fn drop(&mut self) {
        self.shared.canceled.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn start_fuzzy_file_search_session(
    codex_home: PathBuf,
    session_id: String,
    roots: Vec<String>,
    outgoing: Arc<OutgoingMessageSender>,
) -> anyhow::Result<FuzzyFileSearchSession> {
    Ok(FuzzyFileSearchSession {
        shared: Arc::new(SessionShared {
            codex_home,
            roots,
            session_id,
            latest_query: Mutex::new(String::new()),
            outgoing,
            runtime: tokio::runtime::Handle::current(),
            canceled: Arc::new(AtomicBool::new(false)),
        }),
    })
}

struct SessionShared {
    codex_home: PathBuf,
    roots: Vec<String>,
    session_id: String,
    latest_query: Mutex<String>,
    outgoing: Arc<OutgoingMessageSender>,
    runtime: tokio::runtime::Handle,
    canceled: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::run_fuzzy_file_search;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    #[tokio::test]
    async fn run_fuzzy_file_search_returns_graph_backed_matches() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let workspace = TempDir::new()?;
        let source_path = workspace.path().join("src/lib.rs");
        fs::create_dir_all(source_path.parent().expect("source parent"))?;
        fs::write(
            &source_path,
            r#"
pub fn refresh_models_cache() {
    build_graph_snapshot();
}

fn build_graph_snapshot() {}
"#,
        )?;

        let roots = vec![workspace.path().to_string_lossy().into_owned()];
        let initial = run_fuzzy_file_search(
            codex_home.path(),
            "models cache".to_string(),
            roots.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].path, "src/lib.rs");

        fs::remove_file(&source_path)?;

        let cached = run_fuzzy_file_search(
            codex_home.path(),
            "models cache".to_string(),
            roots,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].path, "src/lib.rs");
        Ok(())
    }
}
