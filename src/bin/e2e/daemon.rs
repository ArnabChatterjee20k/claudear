//! Daemon lifecycle management for E2E tests.
//!
//! Supports both native binary and Docker container modes.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Handle to a running daemon instance.
pub enum DaemonHandle {
    Process {
        child: Child,
        log_path: PathBuf,
    },
    Docker {
        container_id: String,
        volume_name: String,
        log_path: PathBuf,
    },
}

impl DaemonHandle {
    pub fn log_path(&self) -> &Path {
        match self {
            DaemonHandle::Process { log_path, .. } => log_path,
            DaemonHandle::Docker { log_path, .. } => log_path,
        }
    }

    pub fn volume_name(&self) -> Option<&str> {
        match self {
            DaemonHandle::Docker { volume_name, .. } => Some(volume_name),
            _ => None,
        }
    }

}

/// Start a native daemon process.
pub fn start_process(
    binary: &str,
    config_path: &Path,
    port: u16,
    log_dir: &Path,
    label: &str,
) -> Result<DaemonHandle> {
    std::fs::create_dir_all(log_dir)?;
    let log_path = log_dir.join(format!("{}.log", label));
    let log_file = std::fs::File::create(&log_path)?;
    let log_stderr = log_file.try_clone()?;

    let child = Command::new(binary)
        .args([
            "--config",
            config_path.to_str().unwrap_or(""),
            "start",
            "--poll",
            "--poll-interval",
            "5000",
            "--port",
            &port.to_string(),
            "--no-webhooks",
        ])
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_stderr))
        .spawn()
        .context("spawn claudear daemon")?;

    tracing::info!(pid = child.id(), port, label, "Started daemon process");
    Ok(DaemonHandle::Process { child, log_path })
}

/// Try to extract Claude Code OAuth token from the macOS keychain.
fn get_keychain_oauth_token() -> Option<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let creds_json = String::from_utf8(output.stdout).ok()?;
    let creds_json = creds_json.trim();
    if creds_json.is_empty() {
        return None;
    }

    // Parse JSON: {"claudeAiOauth":{"accessToken":"sk-ant-...",...}}
    let parsed: serde_json::Value = serde_json::from_str(creds_json).ok()?;
    let token = parsed
        .get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()?
        .to_string();

    if token.is_empty() {
        return None;
    }

    tracing::info!("Extracted Claude Code OAuth token from macOS keychain");
    Some(token)
}

/// Start a Docker daemon container.
pub fn start_docker(
    image: &str,
    config_path: &Path,
    port: u16,
    log_dir: &Path,
    label: &str,
    volume_name: Option<&str>,
    repos_dir: Option<&Path>,
) -> Result<DaemonHandle> {
    std::fs::create_dir_all(log_dir)?;
    let log_path = log_dir.join(format!("{}.log", label));

    let default_vol = format!("claudear-e2e-db-{}", port);
    let vol = volume_name.unwrap_or(&default_vol);

    // Ensure volume exists
    let _ = Command::new("docker")
        .args(["volume", "create", vol])
        .output();

    // Build docker args dynamically
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        format!("claudear-e2e-{}", label),
        "-p".to_string(),
        format!("{}:{}", port, port),
        "-v".to_string(),
        format!("{}:/app/config.toml:ro", config_path.display()),
        "-v".to_string(),
        format!("{}:/app/data", vol),
    ];

    // Mount host repos dir into container
    if let Some(repos) = repos_dir {
        args.extend(["-v".to_string(), format!("{}:/app/repos", repos.display())]);
    }

    // Pass through env vars for Claude auth
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            args.extend(["-e".to_string(), format!("ANTHROPIC_API_KEY={}", key)]);
        }
    }

    // Pass GitHub token for gh CLI usage inside container
    if let Ok(gh_token) = std::env::var("CLAUDEAR_E2E_GITHUB_TOKEN") {
        if !gh_token.is_empty() {
            args.extend(["-e".to_string(), format!("GH_TOKEN={}", gh_token)]);
        }
    }

    // Try CLAUDE_CODE_OAUTH_TOKEN from env, then fall back to macOS keychain
    let oauth_token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(get_keychain_oauth_token);
    if let Some(token) = oauth_token {
        args.extend([
            "-e".to_string(),
            format!("CLAUDE_CODE_OAUTH_TOKEN={}", token),
        ]);
    }

    args.extend([
        image.to_string(),
        "claudear".to_string(),
        "--config".to_string(),
        "/app/config.toml".to_string(),
        "start".to_string(),
        "--poll".to_string(),
        "--poll-interval".to_string(),
        "5000".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--no-webhooks".to_string(),
    ]);

    let container_id = {
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = Command::new("docker")
            .args(&arg_refs)
            .output()
            .context("docker run")?;

        if !output.status.success() {
            bail!(
                "Docker run failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    // Stream Docker container logs to the log file so wait_for_log_message works.
    {
        let log_file = std::fs::File::create(&log_path).context("create docker log file")?;
        let log_stderr = log_file.try_clone().context("clone docker log file")?;
        let cid = container_id.clone();
        std::thread::spawn(move || {
            let _ = Command::new("docker")
                .args(["logs", "-f", &cid])
                .stdout(log_file)
                .stderr(log_stderr)
                .status();
        });
    }

    tracing::info!(container = %container_id, port, label, "Started Docker container");
    Ok(DaemonHandle::Docker {
        container_id,
        volume_name: vol.to_string(),
        log_path,
    })
}

/// Stop a daemon instance.
pub fn stop(handle: &mut DaemonHandle) {
    match handle {
        DaemonHandle::Process { child, .. } => {
            let _ = child.kill();
            let _ = child.wait();
        }
        DaemonHandle::Docker { container_id, .. } => {
            let _ = Command::new("docker").args(["stop", container_id]).output();
            let _ = Command::new("docker")
                .args(["rm", "-f", container_id])
                .output();
        }
    }
}

/// Wait for the daemon's health endpoint to respond.
pub async fn wait_healthy(port: u16, timeout: Duration) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/api/health", port);
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            bail!(
                "Daemon on port {} did not become healthy within {:?}",
                port,
                timeout
            );
        }

        match client.get(&url).send().await {
            Ok(_) => return Ok(()), // Any HTTP response means the server is up
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
}

