use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::workflow::{LoadedWorkflow, load};

#[derive(Clone)]
pub struct WorkflowStore {
    inner: Arc<RwLock<State>>,
}

#[derive(Clone)]
struct State {
    path: PathBuf,
    stamp: Option<FileStamp>,
    current: LoadedWorkflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    mtime_secs: i64,
    size: u64,
    content_hash: u64,
}

impl WorkflowStore {
    pub async fn new(path: PathBuf) -> Result<Self> {
        let workflow = load(&path)?;
        let stamp = current_stamp(&path).ok();
        Ok(Self {
            inner: Arc::new(RwLock::new(State {
                path,
                stamp,
                current: workflow,
            })),
        })
    }

    pub async fn current(&self) -> LoadedWorkflow {
        self.inner.read().await.current.clone()
    }

    pub async fn path(&self) -> PathBuf {
        self.inner.read().await.path.clone()
    }

    pub async fn force_reload(&self) -> Result<()> {
        let path = self.path().await;
        let workflow = load(&path)?;
        let stamp = current_stamp(&path).ok();
        let mut guard = self.inner.write().await;
        guard.current = workflow;
        guard.stamp = stamp;
        Ok(())
    }

    pub async fn maybe_reload(&self) -> Result<bool> {
        let path = self.path().await;
        let stamp = current_stamp(&path)?;
        let current_stamp_value = self.inner.read().await.stamp;
        if current_stamp_value == Some(stamp) {
            return Ok(false);
        }

        let workflow = load(&path)?;
        let mut guard = self.inner.write().await;
        guard.current = workflow;
        guard.stamp = Some(stamp);
        Ok(true)
    }

    pub fn spawn_reload_task(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                if let Err(error) = store.maybe_reload().await {
                    tracing::error!("Failed to reload workflow: {error}");
                }
            }
        });
    }
}

fn current_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified()?;
    let mtime_secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let content = fs::read(path)?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    Ok(FileStamp {
        mtime_secs,
        size: metadata.len(),
        content_hash: hasher.finish(),
    })
}
