use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::config::{CliOverrides, LoadedConfig, Settings};

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<State>>,
}

#[derive(Clone)]
struct State {
    config_path: PathBuf,
    workflow_path: PathBuf,
    overrides: CliOverrides,
    stamp: SourceStamp,
    current: LoadedConfig,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SourceStamp {
    config: Option<FileStamp>,
    workflow: Option<FileStamp>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    mtime_secs: i64,
    size: u64,
    content_hash: u64,
}

impl ConfigStore {
    pub async fn new(
        config_path: PathBuf,
        workflow_path: PathBuf,
        overrides: CliOverrides,
    ) -> Result<Self> {
        let current = Settings::load(&config_path, Some(&workflow_path), &overrides)
            .map_err(anyhow::Error::new)?;
        let stamp = current_stamp(&config_path, &workflow_path);
        Ok(Self {
            inner: Arc::new(RwLock::new(State {
                config_path,
                workflow_path,
                overrides,
                stamp,
                current,
            })),
        })
    }

    pub async fn current(&self) -> LoadedConfig {
        self.inner.read().await.current.clone()
    }

    pub async fn current_settings(&self) -> Settings {
        self.inner.read().await.current.settings.clone()
    }

    pub async fn workflow_path(&self) -> PathBuf {
        self.inner.read().await.workflow_path.clone()
    }

    pub async fn maybe_reload(&self) -> Result<bool> {
        let (config_path, workflow_path, overrides, previous_stamp) = {
            let state = self.inner.read().await;
            (
                state.config_path.clone(),
                state.workflow_path.clone(),
                state.overrides.clone(),
                state.stamp,
            )
        };
        let stamp = current_stamp(&config_path, &workflow_path);
        if stamp == previous_stamp {
            return Ok(false);
        }

        let resolved = Settings::load(&config_path, Some(&workflow_path), &overrides)
            .map_err(anyhow::Error::new)?;
        let mut guard = self.inner.write().await;
        guard.current = resolved;
        guard.stamp = stamp;
        Ok(true)
    }

    pub fn spawn_reload_task(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                match store.maybe_reload().await {
                    Ok(true) => {
                        let resolved = store.current().await;
                        for warning in &resolved.warnings {
                            tracing::warn!("{warning}");
                        }
                    }
                    Ok(false) => {}
                    Err(error) => tracing::error!("Failed to reload config: {error}"),
                }
            }
        });
    }
}

fn current_stamp(config_path: &Path, workflow_path: &Path) -> SourceStamp {
    SourceStamp {
        config: single_file_stamp(config_path).ok(),
        workflow: single_file_stamp(workflow_path).ok(),
    }
}

fn single_file_stamp(path: &Path) -> Result<FileStamp> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn maybe_reload_updates_settings_when_config_changes() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        let workflow_path = temp.path().join("WORKFLOW.md");
        fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\n[polling]\ninterval_ms = 1000\n",
        )
        .unwrap();
        fs::write(&workflow_path, "Prompt body\n").unwrap();

        let store = ConfigStore::new(config_path.clone(), workflow_path, CliOverrides::default())
            .await
            .unwrap();
        assert_eq!(store.current_settings().await.polling.interval_ms, 1000);

        fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\n[polling]\ninterval_ms = 2500\n",
        )
        .unwrap();

        let reloaded = store.maybe_reload().await.unwrap();
        assert!(reloaded);
        assert_eq!(store.current_settings().await.polling.interval_ms, 2500);
    }

    #[tokio::test]
    async fn maybe_reload_keeps_last_good_settings_on_invalid_config() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        let workflow_path = temp.path().join("WORKFLOW.md");
        fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\n[polling]\ninterval_ms = 1000\n",
        )
        .unwrap();
        fs::write(&workflow_path, "Prompt body\n").unwrap();

        let store = ConfigStore::new(config_path.clone(), workflow_path, CliOverrides::default())
            .await
            .unwrap();

        fs::write(&config_path, "[tracker]\nkind = [\n").unwrap();

        assert!(store.maybe_reload().await.is_err());
        assert_eq!(store.current_settings().await.polling.interval_ms, 1000);
    }
}
