use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Result, anyhow, bail};
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::Settings;
use crate::ssh;

#[derive(Clone, Debug)]
pub struct WorkspaceContext {
    pub path: PathBuf,
    pub created_now: bool,
    pub worker_host: Option<String>,
}

pub async fn create_for_issue(
    issue_identifier: &str,
    settings: &Settings,
    worker_host: Option<&str>,
) -> Result<WorkspaceContext> {
    let workspace_key = sanitize_identifier(issue_identifier);
    if let Some(host) = worker_host {
        let path = settings.workspace.root.join(&workspace_key);
        ensure_remote_workspace(&path, host, settings).await?;
        run_after_create_if_needed(&path, true, settings, worker_host).await?;
        return Ok(WorkspaceContext {
            path,
            created_now: true,
            worker_host: Some(host.to_string()),
        });
    }

    fs::create_dir_all(&settings.workspace.root)?;
    let canonical_root = settings
        .workspace
        .root
        .canonicalize()
        .or_else(|_| Ok::<PathBuf, std::io::Error>(settings.workspace.root.clone()))?;
    let target = canonical_root.join(workspace_key);
    let created_now = if target.is_dir() {
        false
    } else if target.exists() {
        fs::remove_file(&target).or_else(|_| fs::remove_dir_all(&target))?;
        fs::create_dir_all(&target)?;
        true
    } else {
        fs::create_dir_all(&target)?;
        true
    };
    validate_local_workspace_path(&canonical_root, &target)?;
    run_after_create_if_needed(&target, created_now, settings, worker_host).await?;
    Ok(WorkspaceContext {
        path: target,
        created_now,
        worker_host: None,
    })
}

pub async fn run_before_run_hook(
    workspace: &WorkspaceContext,
    issue_identifier: &str,
    settings: &Settings,
) -> Result<()> {
    if let Some(command) = settings.hooks.before_run.as_deref() {
        run_hook(
            command,
            workspace,
            issue_identifier,
            "before_run",
            false,
            settings,
        )
        .await?;
    }
    Ok(())
}

pub async fn run_after_run_hook(
    workspace: &WorkspaceContext,
    issue_identifier: &str,
    settings: &Settings,
) {
    if let Some(command) = settings.hooks.after_run.as_deref()
        && let Err(error) = run_hook(
            command,
            workspace,
            issue_identifier,
            "after_run",
            true,
            settings,
        )
        .await
    {
        tracing::warn!("after_run hook failed for {issue_identifier}: {error}");
    }
}

pub async fn remove_issue_workspace(
    issue_identifier: &str,
    settings: &Settings,
    worker_host: Option<&str>,
) -> Result<()> {
    let workspace_key = sanitize_identifier(issue_identifier);
    let path = settings.workspace.root.join(workspace_key);
    let ctx = WorkspaceContext {
        path,
        created_now: false,
        worker_host: worker_host.map(ToString::to_string),
    };
    remove_workspace(&ctx, issue_identifier, settings).await
}

pub async fn remove_workspace(
    workspace: &WorkspaceContext,
    issue_identifier: &str,
    settings: &Settings,
) -> Result<()> {
    if let Some(command) = settings.hooks.before_remove.as_deref()
        && let Err(error) = run_hook(
            command,
            workspace,
            issue_identifier,
            "before_remove",
            true,
            settings,
        )
        .await
    {
        tracing::warn!("before_remove hook failed for {issue_identifier}: {error}");
    }

    match workspace.worker_host.as_deref() {
        Some(host) => {
            let command = format!(
                "rm -rf {}",
                ssh::shell_escape(&workspace.path.to_string_lossy())
            );
            let (_output, status) = ssh::run(host, &command).await?;
            if status != 0 {
                bail!("workspace_remove_failed");
            }
            Ok(())
        }
        None => {
            let root = settings
                .workspace
                .root
                .canonicalize()
                .unwrap_or_else(|_| settings.workspace.root.clone());
            let target = workspace
                .path
                .canonicalize()
                .unwrap_or_else(|_| workspace.path.clone());
            if target == root {
                bail!("cannot_remove_workspace_root");
            }
            if target.exists() {
                fs::remove_dir_all(target)?;
            }
            Ok(())
        }
    }
}

pub fn sanitize_identifier(identifier: &str) -> String {
    identifier
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

async fn run_after_create_if_needed(
    workspace: &Path,
    created_now: bool,
    settings: &Settings,
    worker_host: Option<&str>,
) -> Result<()> {
    if created_now && let Some(command) = settings.hooks.after_create.as_deref() {
        let ctx = WorkspaceContext {
            path: workspace.to_path_buf(),
            created_now,
            worker_host: worker_host.map(ToString::to_string),
        };
        run_hook(
            command,
            &ctx,
            &workspace.display().to_string(),
            "after_create",
            false,
            settings,
        )
        .await?;
    }
    Ok(())
}

async fn run_hook(
    command: &str,
    workspace: &WorkspaceContext,
    issue_identifier: &str,
    hook_name: &str,
    best_effort: bool,
    settings: &Settings,
) -> Result<()> {
    let timeout_duration = std::time::Duration::from_millis(settings.hooks.timeout_ms);
    let result = match workspace.worker_host.as_deref() {
        Some(host) => {
            let command = format!(
                "cd {} && {}",
                ssh::shell_escape(&workspace.path.to_string_lossy()),
                command
            );
            timeout(timeout_duration, ssh::run(host, &command)).await
        }
        None => {
            let mut child = Command::new("sh");
            child.kill_on_drop(true);
            child.arg("-lc").arg(command);
            child.current_dir(&workspace.path);
            child.stdout(Stdio::piped());
            child.stderr(Stdio::piped());
            timeout(timeout_duration, async move {
                let output = child.output().await?;
                let merged = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                Ok::<_, anyhow::Error>((merged, output.status.code().unwrap_or_default()))
            })
            .await
        }
    };

    match result {
        Ok(Ok((_output, 0))) => Ok(()),
        Ok(Ok((output, status))) => {
            if best_effort {
                tracing::warn!(
                    "Workspace hook failed hook={hook_name} issue={issue_identifier} status={status} output={output}"
                );
                Ok(())
            } else {
                bail!("workspace_hook_failed: {hook_name}: status={status} output={output}");
            }
        }
        Ok(Err(error)) => {
            if best_effort {
                tracing::warn!(
                    "Workspace hook failed hook={hook_name} issue={issue_identifier} error={error}"
                );
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(_) => {
            if best_effort {
                tracing::warn!(
                    "Workspace hook timed out hook={hook_name} issue={issue_identifier}"
                );
                Ok(())
            } else {
                bail!("workspace_hook_timeout: {hook_name}");
            }
        }
    }
}

fn validate_local_workspace_path(root: &Path, workspace: &Path) -> Result<()> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| anyhow!("path_canonicalize_failed: {error}"))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| anyhow!("path_canonicalize_failed: {error}"))?;
    if canonical_workspace == canonical_root {
        bail!("invalid_workspace_cwd");
    }
    if !canonical_workspace.starts_with(&canonical_root) {
        bail!("workspace_outside_root");
    }
    Ok(())
}

async fn ensure_remote_workspace(
    path: &Path,
    worker_host: &str,
    settings: &Settings,
) -> Result<()> {
    let command = format!(
        "set -eu\nworkspace={}\nif [ -d \"$workspace\" ]; then exit 0; fi\nif [ -e \"$workspace\" ]; then rm -rf \"$workspace\"; fi\nmkdir -p \"$workspace\"",
        ssh::shell_escape(&path.to_string_lossy())
    );
    let (output, status) = timeout(
        std::time::Duration::from_millis(settings.hooks.timeout_ms),
        ssh::run(worker_host, &command),
    )
    .await
    .map_err(|_| anyhow!("workspace_prepare_timeout"))??;
    if status != 0 {
        bail!("workspace_prepare_failed: {output}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, settings_from_toml_str};

    fn settings(root: &Path) -> Settings {
        settings_from_toml_str(&format!(
            "[tracker]\nkind = \"memory\"\n[workspace]\nroot = \"{}\"\n",
            root.display()
        ))
    }

    #[tokio::test]
    async fn creates_and_reuses_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let first = create_for_issue("MT-1", &settings, None).await.unwrap();
        let second = create_for_issue("MT-1", &settings, None).await.unwrap();
        assert!(first.path.exists());
        assert!(!second.created_now);
    }
}
