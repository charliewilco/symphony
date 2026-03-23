use std::env;
use std::process::Stdio;

use anyhow::{Result, anyhow};
use tokio::process::{Child, Command};

pub fn remote_shell_command(command: &str) -> String {
    format!("bash -lc {}", shell_escape(command))
}

pub fn start_ssh_child(host: &str, command: &str) -> Result<Child> {
    let ssh = find_ssh()?;
    let target = parse_target(host);
    let mut cmd = Command::new(ssh);
    cmd.kill_on_drop(true);
    if let Some(config) = env::var("SYMPHONY_SSH_CONFIG")
        .ok()
        .filter(|value| !value.is_empty())
    {
        cmd.arg("-F").arg(config);
    }
    cmd.arg("-T");
    if let Some(port) = &target.port {
        cmd.arg("-p").arg(port);
    }
    cmd.arg(target.destination)
        .arg(remote_shell_command(command));
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    Ok(cmd.spawn()?)
}

pub async fn run(host: &str, command: &str) -> Result<(String, i32)> {
    let ssh = find_ssh()?;
    let target = parse_target(host);
    let mut cmd = Command::new(ssh);
    if let Some(config) = env::var("SYMPHONY_SSH_CONFIG")
        .ok()
        .filter(|value| !value.is_empty())
    {
        cmd.arg("-F").arg(config);
    }
    cmd.arg("-T");
    if let Some(port) = &target.port {
        cmd.arg("-p").arg(port);
    }
    cmd.arg(target.destination)
        .arg(remote_shell_command(command));
    let output = cmd.output().await?;
    let merged = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((merged, output.status.code().unwrap_or_default()))
}

pub fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn find_ssh() -> Result<String> {
    which("ssh").ok_or_else(|| anyhow!("ssh_not_found"))
}

fn which(executable: &str) -> Option<String> {
    env::var("PATH").ok().and_then(|paths| {
        env::split_paths(&paths).find_map(|path| {
            let candidate = path.join(executable);
            candidate
                .exists()
                .then(|| candidate.to_string_lossy().to_string())
        })
    })
}

struct Target {
    destination: String,
    port: Option<String>,
}

fn parse_target(target: &str) -> Target {
    let trimmed = target.trim();
    if let Some((destination, port)) = trimmed.rsplit_once(':') {
        if destination.contains(':') && !(destination.contains('[') && destination.contains(']')) {
            return Target {
                destination: trimmed.to_string(),
                port: None,
            };
        }
        if !destination.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            return Target {
                destination: destination.to_string(),
                port: Some(port.to_string()),
            };
        }
    }
    Target {
        destination: trimmed.to_string(),
        port: None,
    }
}
