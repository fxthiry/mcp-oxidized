//! Backup operation tracking and polling.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinSet;

use crate::error::OxidizedError;
use crate::oxidized::{Node, OxidizedBackend, OxidizedClient};

const OPERATION_TTL: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_BATCH_NODES: usize = 20;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupState {
    Pending,
    Succeeded,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BackupMetadata {
    pub start: Option<String>,
    pub end: Option<String>,
    pub status: Option<String>,
    pub mtime: Option<String>,
}

impl From<&Node> for BackupMetadata {
    fn from(node: &Node) -> Self {
        let last = node.last.as_ref();
        Self {
            start: last.and_then(|backup| backup.start.clone()),
            end: last.and_then(|backup| backup.end.clone()),
            status: node
                .effective_status()
                .map(str::to_string)
                .or_else(|| last.and_then(|backup| backup.status.clone())),
            mtime: node.mtime.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupOperation {
    pub operation_id: String,
    pub node: String,
    pub completion_state: BackupState,
    pub status: Option<String>,
    pub baseline: BackupMetadata,
    pub latest: BackupMetadata,
    pub mtime_changed: bool,
    pub completed: bool,
    pub message: String,
    #[serde(skip)]
    created_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchBackupResult {
    pub operations: Vec<BackupOperation>,
    pub requested: usize,
    pub completed: usize,
    pub failed: usize,
    pub pending: usize,
}

#[derive(Clone, Default)]
pub struct BackupRegistry {
    operations: Arc<RwLock<HashMap<String, BackupOperation>>>,
}

impl BackupRegistry {
    fn operation_id() -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("backup-{millis:x}-{sequence:x}")
    }

    async fn prune(&self, client: &OxidizedClient) {
        let removed_nodes = {
            let mut operations = self.operations.write().await;
            let mut removed = Vec::new();
            operations.retain(|_, operation| {
                let retain = operation.created_at.elapsed() < OPERATION_TTL;
                if !retain && !operation.completed {
                    removed.push(operation.node.clone());
                }
                retain
            });
            removed
        };
        for node in removed_nodes {
            self.clear_pending_if_idle(client, &node).await;
        }
    }

    async fn clear_pending_if_idle(&self, client: &OxidizedClient, node: &str) {
        let has_pending = self
            .operations
            .read()
            .await
            .values()
            .any(|operation| operation.node == node && !operation.completed);
        if !has_pending {
            client.set_backup_pending(node, false).await;
        }
    }

    pub async fn start(
        &self,
        client: &OxidizedClient,
        node: &str,
        wait: bool,
        timeout_seconds: u64,
    ) -> Result<BackupOperation, OxidizedError> {
        if !(1..=300).contains(&timeout_seconds) {
            return Err(OxidizedError::InvalidRegex(
                "timeout_seconds must be between 1 and 300".to_string(),
            ));
        }
        self.prune(client).await;
        let baseline_node = client.get_node_fresh(node).await?;
        let baseline = BackupMetadata::from(&baseline_node);
        let operation_id = Self::operation_id();
        let operation = BackupOperation {
            operation_id: operation_id.clone(),
            node: node.to_string(),
            completion_state: BackupState::Pending,
            status: Some("queued".to_string()),
            baseline: baseline.clone(),
            latest: baseline,
            mtime_changed: false,
            completed: false,
            message: format!("Backup queued for node '{node}'"),
            created_at: Instant::now(),
        };
        self.operations
            .write()
            .await
            .insert(operation_id.clone(), operation);

        client.set_backup_pending(node, true).await;
        if let Err(error) = client.trigger_backup(node).await {
            self.operations.write().await.remove(&operation_id);
            self.clear_pending_if_idle(client, node).await;
            return Err(error);
        }

        if wait {
            self.wait(client, &operation_id, timeout_seconds).await
        } else {
            let operation = self
                .operations
                .read()
                .await
                .get(&operation_id)
                .expect("operation was inserted")
                .clone();
            let registry = self.clone();
            let client = client.clone();
            let tracked_id = operation_id;
            tokio::spawn(async move {
                if let Err(error) = registry.wait(&client, &tracked_id, timeout_seconds).await {
                    tracing::warn!(
                        operation_id = %tracked_id,
                        error = %error,
                        "Background backup tracking stopped"
                    );
                }
            });
            Ok(operation)
        }
    }

    pub async fn status(
        &self,
        client: &OxidizedClient,
        operation_id: &str,
    ) -> Result<Option<BackupOperation>, OxidizedError> {
        self.prune(client).await;
        let Some(operation) = self.operations.read().await.get(operation_id).cloned() else {
            return Ok(None);
        };
        if operation.completed {
            return Ok(Some(operation));
        }

        let latest_node = client.get_node_fresh(&operation.node).await?;
        let latest = BackupMetadata::from(&latest_node);
        let newer_run = latest.start != operation.baseline.start
            || latest.end != operation.baseline.end
            || latest.status != operation.baseline.status;
        let run_complete = newer_run
            && latest.end.is_some()
            && !matches!(
                latest.status.as_deref(),
                Some("pending" | "running" | "queued")
            );
        let failed = run_complete
            && !matches!(
                latest.status.as_deref(),
                Some("success" | "complete" | "completed")
            );

        let mut updated = operation;
        updated.latest = latest;
        updated.status = updated.latest.status.clone();
        updated.mtime_changed = updated.latest.mtime != updated.baseline.mtime;
        if run_complete {
            updated.completed = true;
            updated.completion_state = if failed {
                BackupState::Failed
            } else {
                BackupState::Succeeded
            };
            updated.message = if failed {
                format!("Backup failed for node '{}'", updated.node)
            } else if updated.mtime_changed {
                format!(
                    "Backup completed with configuration changes for '{}'",
                    updated.node
                )
            } else {
                format!(
                    "Backup completed with unchanged configuration for '{}'",
                    updated.node
                )
            };
            client.invalidate_config(&updated.node).await;
        }
        self.operations
            .write()
            .await
            .insert(operation_id.to_string(), updated.clone());
        if updated.completed {
            self.clear_pending_if_idle(client, &updated.node).await;
        }
        Ok(Some(updated))
    }

    pub async fn wait(
        &self,
        client: &OxidizedClient,
        operation_id: &str,
        timeout_seconds: u64,
    ) -> Result<BackupOperation, OxidizedError> {
        let timeout = Duration::from_secs(timeout_seconds);
        let started = Instant::now();
        loop {
            let operation = self
                .status(client, operation_id)
                .await?
                .expect("operation exists while waiting");
            if operation.completed {
                return Ok(operation);
            }
            if started.elapsed() >= timeout {
                let mut timed_out = operation;
                timed_out.completed = true;
                timed_out.completion_state = BackupState::TimedOut;
                timed_out.message = format!("Timed out waiting for backup of '{}'", timed_out.node);
                self.operations
                    .write()
                    .await
                    .insert(operation_id.to_string(), timed_out.clone());
                self.clear_pending_if_idle(client, &timed_out.node).await;
                return Ok(timed_out);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn start_batch(
        &self,
        client: &OxidizedClient,
        mut nodes: Vec<String>,
        wait: bool,
        timeout_seconds: u64,
        concurrency: usize,
    ) -> Result<BatchBackupResult, OxidizedError> {
        nodes.sort();
        nodes.dedup();
        if nodes.is_empty() {
            return Err(OxidizedError::InvalidRegex(
                "Batch backup requires at least one node".to_string(),
            ));
        }
        if nodes.len() > MAX_BATCH_NODES {
            return Err(OxidizedError::InvalidRegex(format!(
                "Batch backup is limited to {MAX_BATCH_NODES} nodes"
            )));
        }
        if !(1..=10).contains(&concurrency) {
            return Err(OxidizedError::InvalidRegex(
                "concurrency must be between 1 and 10".to_string(),
            ));
        }
        let requested = nodes.len();
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut tasks = JoinSet::new();
        for node in nodes {
            let registry = self.clone();
            let client = client.clone();
            let semaphore = Arc::clone(&semaphore);
            tasks.spawn(async move {
                let _permit =
                    semaphore
                        .acquire_owned()
                        .await
                        .map_err(|_| OxidizedError::HttpError {
                            status_code: 503,
                            context: "batch semaphore closed".to_string(),
                        })?;
                registry.start(&client, &node, wait, timeout_seconds).await
            });
        }

        let mut operations = Vec::new();
        while let Some(task) = tasks.join_next().await {
            operations.push(task.map_err(|error| OxidizedError::HttpError {
                status_code: 500,
                context: format!("batch task failed: {error}"),
            })??);
        }
        operations.sort_by(|a, b| a.node.cmp(&b.node));
        let completed = operations
            .iter()
            .filter(|operation| operation.completed)
            .count();
        let failed = operations
            .iter()
            .filter(|operation| {
                matches!(
                    operation.completion_state,
                    BackupState::Failed | BackupState::TimedOut
                )
            })
            .count();
        let pending = operations
            .iter()
            .filter(|operation| operation.completion_state == BackupState::Pending)
            .count();
        Ok(BatchBackupResult {
            operations,
            requested,
            completed,
            failed,
            pending,
        })
    }
}
