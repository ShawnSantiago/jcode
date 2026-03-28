//! Self-development tool - manage canary builds when working on jcode itself

use crate::background::{self, TaskResult};
use crate::build;
use crate::cli::tui_launch;
use crate::protocol::{ServerEvent, TranscriptMode};
use crate::server;
use crate::session;
use crate::storage;
use crate::tool::{Tool, ToolContext, ToolExecutionMode, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
struct SelfDevInput {
    action: String,
    /// Optional prompt to seed the spawned self-dev session.
    #[serde(default)]
    prompt: Option<String>,
    /// Optional context for reload - what the agent is working on
    #[serde(default)]
    context: Option<String>,
    /// Why this build is needed; shown to other queued/blocked agents.
    #[serde(default)]
    reason: Option<String>,
    /// Whether to notify the requesting agent when the queued background build completes.
    #[serde(default)]
    notify: Option<bool>,
}

/// Context saved before reload, restored after restart
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadContext {
    /// What the agent was working on (user-provided or auto-detected)
    pub task_context: Option<String>,
    /// Version before reload
    pub version_before: String,
    /// New version (target)
    pub version_after: String,
    /// Session ID
    pub session_id: String,
    /// Timestamp
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct SelfDevLaunchResult {
    pub session_id: String,
    pub repo_dir: PathBuf,
    pub launched: bool,
    pub test_mode: bool,
    pub exe: Option<PathBuf>,
}

impl SelfDevLaunchResult {
    pub fn command_preview(&self) -> Option<String> {
        self.exe
            .as_ref()
            .map(|exe| format!("{} --resume {} self-dev", exe.display(), self.session_id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BuildRequestState {
    Queued,
    Building,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildRequest {
    request_id: String,
    background_task_id: Option<String>,
    session_id: String,
    session_short_name: Option<String>,
    session_title: Option<String>,
    reason: String,
    repo_dir: String,
    command: String,
    requested_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    state: BuildRequestState,
    version: Option<String>,
    error: Option<String>,
    output_file: Option<String>,
    status_file: Option<String>,
}

impl BuildRequest {
    fn requests_dir() -> Result<PathBuf> {
        let dir = storage::jcode_dir()?.join("selfdev-build-requests");
        storage::ensure_dir(&dir)?;
        Ok(dir)
    }

    fn path_for_request(request_id: &str) -> Result<PathBuf> {
        Ok(Self::requests_dir()?.join(format!("{}.json", request_id)))
    }

    fn save(&self) -> Result<()> {
        storage::write_json(&Self::path_for_request(&self.request_id)?, self)
    }

    fn load(request_id: &str) -> Result<Option<Self>> {
        let path = Self::path_for_request(request_id)?;
        if path.exists() {
            Ok(Some(storage::read_json(&path)?))
        } else {
            Ok(None)
        }
    }

    fn load_all() -> Result<Vec<Self>> {
        let dir = Self::requests_dir()?;
        let mut requests = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(request) = storage::read_json::<Self>(&path) {
                requests.push(request);
            }
        }
        requests.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        Ok(requests)
    }

    fn pending_requests() -> Result<Vec<Self>> {
        Ok(Self::load_all()?
            .into_iter()
            .filter(|request| {
                matches!(
                    request.state,
                    BuildRequestState::Queued | BuildRequestState::Building
                )
            })
            .collect())
    }

    fn display_owner(&self) -> String {
        if let Some(short_name) = self.session_short_name.as_deref() {
            return format!("{} ({})", short_name, self.session_id);
        }
        if let Some(title) = self.session_title.as_deref() {
            return format!("{} ({})", title, self.session_id);
        }
        self.session_id.clone()
    }
}

struct BuildLockGuard {
    _file: std::fs::File,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct SelfDevBuildCommand {
    program: String,
    args: Vec<String>,
    display: String,
}

#[cfg(unix)]
impl Drop for BuildLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn enter_selfdev_session(working_dir: Option<&Path>) -> Result<SelfDevLaunchResult> {
    let repo_dir = SelfDevTool::resolve_repo_dir(working_dir).ok_or_else(|| {
        anyhow::anyhow!("Could not find the jcode repository to enter self-dev mode")
    })?;

    let mut session = session::Session::create(None, Some("Self-development session".to_string()));
    session.set_canary("self-dev");
    session.working_dir = Some(repo_dir.display().to_string());
    session.save()?;

    let session_id = session.id.clone();

    if SelfDevTool::is_test_session() {
        return Ok(SelfDevLaunchResult {
            session_id,
            repo_dir,
            launched: false,
            test_mode: true,
            exe: None,
        });
    }

    let exe = SelfDevTool::launch_binary()?;
    let launched = tui_launch::spawn_selfdev_in_new_terminal(&exe, &session_id, &repo_dir)?;

    Ok(SelfDevLaunchResult {
        session_id,
        repo_dir,
        launched,
        test_mode: false,
        exe: Some(exe),
    })
}

pub fn schedule_selfdev_prompt_delivery(session_id: String, prompt: String) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match runtime {
            Ok(runtime) => {
                if let Err(err) =
                    runtime.block_on(SelfDevTool::send_prompt_to_session(&session_id, &prompt))
                {
                    crate::logging::warn(&format!(
                        "Failed to auto-deliver prompt to spawned self-dev session {}: {}",
                        session_id, err
                    ));
                }
            }
            Err(err) => crate::logging::warn(&format!(
                "Failed to initialize runtime for self-dev prompt delivery: {}",
                err
            )),
        }
    });
}

pub fn selfdev_status_output() -> Result<ToolOutput> {
    let manifest = build::BuildManifest::load()?;

    let mut status = String::new();

    status.push_str("## Current Version\n\n");
    status.push_str(&format!("**Running:** jcode {}\n", env!("JCODE_VERSION")));

    if let Some(repo_dir) = build::get_repo_dir() {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo_dir)
            .output()
            .ok();

        if let Some(output) = output {
            let changes: Vec<&str> = std::str::from_utf8(&output.stdout)
                .unwrap_or("")
                .lines()
                .collect();
            if changes.is_empty() {
                status.push_str("**Working tree:** clean\n");
            } else {
                status.push_str(&format!(
                    "**Working tree:** {} uncommitted change{}\n",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                ));
            }
        }
    }

    status.push_str("\n## Build Channels\n\n");

    if let Ok(Some(current)) = build::read_current_version() {
        status.push_str(&format!("**Current:** {}\n", current));
    } else {
        status.push_str("**Current:** none\n");
    }

    if let Some(ref stable) = manifest.stable {
        status.push_str(&format!("**Stable:** {}\n", stable));
    } else {
        status.push_str("**Stable:** none\n");
    }

    if let Some(ref canary) = manifest.canary {
        let status_str = match &manifest.canary_status {
            Some(build::CanaryStatus::Testing) => "testing",
            Some(build::CanaryStatus::Passed) => "passed",
            Some(build::CanaryStatus::Failed) => "failed",
            None => "unknown",
        };
        status.push_str(&format!("**Canary:** {} ({})\n", canary, status_str));
    } else {
        status.push_str("**Canary:** none\n");
    }

    status.push_str("\n## Debug Socket\n\n");
    status.push_str(&format!(
        "**Path:** {}\n",
        server::debug_socket_path().display()
    ));

    if let Some(reload_state) = server::ReloadState::load() {
        status.push_str("\n## Reload State\n\n");
        status.push_str(&format!(
            "**Phase:** {:?}\n**Request:** {}\n**Hash:** {}\n**PID:** {}\n**Timestamp:** {}\n",
            reload_state.phase,
            reload_state.request_id,
            reload_state.hash,
            reload_state.pid,
            reload_state.timestamp,
        ));
        if let Some(detail) = reload_state.detail {
            status.push_str(&format!("**Detail:** {}\n", detail));
        }
    }

    let pending_requests = BuildRequest::pending_requests()?;
    if !pending_requests.is_empty() {
        status.push_str("\n## Build Queue\n\n");
        for (index, request) in pending_requests.iter().enumerate() {
            let state = match request.state {
                BuildRequestState::Queued => "queued",
                BuildRequestState::Building => "building",
                BuildRequestState::Completed => "completed",
                BuildRequestState::Failed => "failed",
            };
            status.push_str(&format!(
                "{}. **{}** — {}\n   Reason: {}\n   Requested: {}\n",
                index + 1,
                state,
                request.display_owner(),
                request.reason,
                request.requested_at,
            ));
            if let Some(task_id) = request.background_task_id.as_deref() {
                status.push_str(&format!("   Task: `{}`\n", task_id));
            }
            if let Some(started_at) = request.started_at.as_deref() {
                status.push_str(&format!("   Started: {}\n", started_at));
            }
        }
    }

    if let Some(ref crash) = manifest.last_crash {
        status.push_str(&format!(
            "\n## Last Crash\n\n\
             Build: {}\n\
             Exit code: {}\n\
             Time: {}\n",
            crash.build_hash,
            crash.exit_code,
            crash.crashed_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        if !crash.stderr.is_empty() {
            let stderr_preview = if crash.stderr.len() > 500 {
                format!("{}...", crate::util::truncate_str(&crash.stderr, 500))
            } else {
                crash.stderr.clone()
            };
            status.push_str(&format!("\nStderr:\n```\n{}\n```\n", stderr_preview));
        }
    }

    if !manifest.history.is_empty() {
        status.push_str("\n## Recent Builds\n\n");
        for (i, info) in manifest.history.iter().take(5).enumerate() {
            let dirty_marker = if info.dirty { " (dirty)" } else { "" };
            let msg = info
                .commit_message
                .as_deref()
                .unwrap_or("No commit message");
            status.push_str(&format!(
                "{}. `{}`{} - {}\n   Built: {}\n",
                i + 1,
                info.hash,
                dirty_marker,
                msg,
                info.built_at.format("%Y-%m-%d %H:%M:%S UTC")
            ));
        }
    }

    Ok(ToolOutput::new(status))
}

impl ReloadContext {
    fn sanitize_session_id(session_id: &str) -> String {
        session_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    }

    pub fn path_for_session(session_id: &str) -> Result<std::path::PathBuf> {
        let sanitized = Self::sanitize_session_id(session_id);
        Ok(storage::jcode_dir()?.join(format!("reload-context-{}.json", sanitized)))
    }

    fn legacy_path() -> Result<std::path::PathBuf> {
        Ok(storage::jcode_dir()?.join("reload-context.json"))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path_for_session(&self.session_id)?;
        storage::write_json(&path, self)?;
        Ok(())
    }

    pub fn load() -> Result<Option<Self>> {
        let legacy = Self::legacy_path()?;
        if !legacy.exists() {
            return Ok(None);
        }
        let ctx: Self = storage::read_json(&legacy)?;
        let _ = std::fs::remove_file(&legacy);
        Ok(Some(ctx))
    }

    /// Peek at context for a specific session without consuming it.
    pub fn peek_for_session(session_id: &str) -> Result<Option<Self>> {
        let session_path = Self::path_for_session(session_id)?;
        if session_path.exists() {
            let ctx: Self = storage::read_json(&session_path)?;
            return Ok(Some(ctx));
        }

        let legacy = Self::legacy_path()?;
        if !legacy.exists() {
            return Ok(None);
        }

        let ctx: Self = storage::read_json(&legacy)?;
        if ctx.session_id == session_id {
            Ok(Some(ctx))
        } else {
            Ok(None)
        }
    }

    /// Load context only if it belongs to the given session; consumes on success.
    pub fn load_for_session(session_id: &str) -> Result<Option<Self>> {
        let session_path = Self::path_for_session(session_id)?;
        if session_path.exists() {
            let ctx: Self = storage::read_json(&session_path)?;
            let _ = std::fs::remove_file(&session_path);
            return Ok(Some(ctx));
        }

        let legacy = Self::legacy_path()?;
        if !legacy.exists() {
            return Ok(None);
        }

        let ctx: Self = storage::read_json(&legacy)?;
        if ctx.session_id == session_id {
            let _ = std::fs::remove_file(&legacy);
            Ok(Some(ctx))
        } else {
            Ok(None)
        }
    }
}

pub struct SelfDevTool;

impl SelfDevTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SelfDevTool {
    fn name(&self) -> &str {
        "selfdev"
    }

    fn description(&self) -> &str {
        "Self-development tool for working on jcode itself. Actions: 'enter' (spawn a new self-dev session), \
         'build' (queue a background self-dev build with a reason/comment), 'status' (show build versions), \
         and in self-dev mode also 'reload', 'socket-info', and 'socket-help'."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                        "enum": [
                        "enter",
                        "build",
                        "reload",
                        "status",
                        "socket-info",
                        "socket-help"
                    ],
                    "description": "Action to perform: 'enter' spawns a new self-dev session, \
                                   'build' queues a coordinated background build with a reason/comment, \
                                   'reload' restarts with built binary, \
                                   'status' shows build versions and crash history, \
                                   'socket-info' returns debug socket paths and connection info, \
                                   'socket-help' shows available debug socket commands"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional prompt to send into the spawned self-dev session after it opens"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context for reload - describe what you're working on. \
                                   This will be included in the continuation message after restart."
                },
                "reason": {
                    "type": "string",
                    "description": "Why this self-dev build is needed. Required for action='build' so other queued agents can see the reason."
                },
                "notify": {
                    "type": "boolean",
                    "description": "For action='build': notify the requesting agent when the queued background build completes (default: true)."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SelfDevInput = serde_json::from_value(input)?;
        let action = params.action.clone();

        let title = format!("selfdev {}", action);

        let result = match action.as_str() {
            "enter" => self.do_enter(params.prompt, &ctx).await,
            "build" => self.do_build(params.reason, params.notify, &ctx).await,
            "reload" => {
                if !SelfDevTool::session_is_selfdev(&ctx.session_id) {
                    Ok(ToolOutput::new(
                        "`selfdev reload` is only available inside a self-dev session. Use `selfdev enter` first.",
                    ))
                } else {
                    self.do_reload(params.context, &ctx.session_id, ctx.execution_mode)
                        .await
                }
            }
            "status" => self.do_status().await,
            "socket-info" => {
                if !SelfDevTool::session_is_selfdev(&ctx.session_id) {
                    Ok(ToolOutput::new(
                        "`selfdev socket-info` is only available inside a self-dev session. Use `selfdev enter` first.",
                    ))
                } else {
                    self.do_socket_info().await
                }
            }
            "socket-help" => {
                if !SelfDevTool::session_is_selfdev(&ctx.session_id) {
                    Ok(ToolOutput::new(
                        "`selfdev socket-help` is only available inside a self-dev session. Use `selfdev enter` first.",
                    ))
                } else {
                    self.do_socket_help().await
                }
            }
            _ => Ok(ToolOutput::new(format!(
                "Unknown action: {}. Use 'enter', 'build', 'reload', 'status', 'socket-info', or 'socket-help'.",
                action
            ))),
        };

        result.map(|output| output.with_title(title))
    }
}

impl SelfDevTool {
    fn is_test_session() -> bool {
        std::env::var("JCODE_TEST_SESSION")
            .map(|value| {
                let trimmed = value.trim();
                !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    }

    fn reload_timeout_secs() -> u64 {
        std::env::var("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .unwrap_or(15)
    }

    fn session_is_selfdev(session_id: &str) -> bool {
        session::Session::load(session_id)
            .map(|session| session.is_canary)
            .unwrap_or(false)
    }

    fn resolve_repo_dir(working_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
        if let Some(dir) = working_dir {
            for ancestor in dir.ancestors() {
                if build::is_jcode_repo(ancestor) {
                    return Some(ancestor.to_path_buf());
                }
            }
        }

        build::get_repo_dir()
    }

    fn launch_binary() -> Result<std::path::PathBuf> {
        build::client_update_candidate(true)
            .map(|(path, _label)| path)
            .or_else(|| std::env::current_exe().ok())
            .ok_or_else(|| anyhow::anyhow!("Could not resolve jcode executable to launch"))
    }

    fn build_command(repo_dir: &Path) -> SelfDevBuildCommand {
        let wrapper = repo_dir.join("scripts").join("dev_cargo.sh");
        if wrapper.is_file() {
            return SelfDevBuildCommand {
                program: "bash".to_string(),
                args: vec![
                    wrapper.to_string_lossy().into_owned(),
                    "build".to_string(),
                    "--release".to_string(),
                    "--bin".to_string(),
                    "jcode".to_string(),
                ],
                display: "scripts/dev_cargo.sh build --release --bin jcode".to_string(),
            };
        }

        SelfDevBuildCommand {
            program: "cargo".to_string(),
            args: vec![
                "build".to_string(),
                "--release".to_string(),
                "--bin".to_string(),
                "jcode".to_string(),
            ],
            display: "cargo build --release --bin jcode".to_string(),
        }
    }

    fn build_lock_path() -> Result<PathBuf> {
        Ok(storage::jcode_dir()?.join("selfdev-build.lock"))
    }

    #[cfg(unix)]
    fn try_acquire_build_lock() -> Result<Option<BuildLockGuard>> {
        use std::fs::OpenOptions;
        use std::os::fd::AsRawFd;

        let path = Self::build_lock_path()?;
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            Ok(Some(BuildLockGuard { _file: file, path }))
        } else {
            Ok(None)
        }
    }

    #[cfg(not(unix))]
    fn try_acquire_build_lock() -> Result<Option<BuildLockGuard>> {
        use std::fs::OpenOptions;

        let path = Self::build_lock_path()?;
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => Ok(Some(BuildLockGuard { _file: file, path })),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn load_session_labels(session_id: &str) -> (Option<String>, Option<String>) {
        session::Session::load(session_id)
            .map(|session| (session.short_name, session.title))
            .unwrap_or((None, None))
    }

    fn newest_active_request() -> Result<Option<BuildRequest>> {
        Ok(BuildRequest::pending_requests()?
            .into_iter()
            .find(|request| request.state == BuildRequestState::Building))
    }

    fn next_request_id() -> String {
        format!("selfdev-build-{}", uuid::Uuid::new_v4().simple())
    }

    fn current_queue_position(request_id: &str) -> Result<Option<usize>> {
        Ok(BuildRequest::pending_requests()?
            .into_iter()
            .position(|request| request.request_id == request_id)
            .map(|index| index + 1))
    }

    async fn append_output_line(file: &mut tokio::fs::File, line: impl AsRef<str>) {
        let _ = file.write_all(line.as_ref().as_bytes()).await;
        let _ = file.write_all(b"\n").await;
        let _ = file.flush().await;
    }

    async fn wait_for_turn(request_id: &str, file: &mut tokio::fs::File) -> Result<BuildLockGuard> {
        let mut last_note: Option<String> = None;
        loop {
            let pending = BuildRequest::pending_requests()?;
            let my_index = pending
                .iter()
                .position(|request| request.request_id == request_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Queued build request {} disappeared", request_id)
                })?;

            if my_index == 0 {
                if let Some(lock) = Self::try_acquire_build_lock()? {
                    return Ok(lock);
                }
            }

            let note = if my_index == 0 {
                Some("Waiting for the self-dev build lock to become available".to_string())
            } else {
                pending.get(my_index - 1).map(|request| {
                    format!(
                        "Waiting in queue behind {} — {}",
                        request.display_owner(),
                        request.reason
                    )
                })
            };
            if note.as_ref() != last_note.as_ref() {
                if let Some(note) = note.as_ref() {
                    Self::append_output_line(file, note).await;
                }
                last_note = note;
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    async fn stream_build_command(
        repo_dir: PathBuf,
        command: SelfDevBuildCommand,
        output_path: PathBuf,
    ) -> Result<TaskResult> {
        let mut cmd = tokio::process::Command::new(&command.program);
        cmd.args(&command.args)
            .current_dir(repo_dir)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn build command: {}", e))?;

        let mut file = tokio::fs::File::create(&output_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create output file: {}", e))?;
        Self::append_output_line(
            &mut file,
            format!("Starting build with {}", command.display),
        )
        .await;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut stdout_lines = stdout.map(|s| BufReader::new(s).lines());
        let mut stderr_lines = stderr.map(|s| BufReader::new(s).lines());
        let mut stdout_done = stdout_lines.is_none();
        let mut stderr_done = stderr_lines.is_none();

        while !stdout_done || !stderr_done {
            tokio::select! {
                line = async {
                    match stdout_lines.as_mut() {
                        Some(r) => r.next_line().await,
                        None => std::future::pending().await,
                    }
                }, if !stdout_done => {
                    match line {
                        Ok(Some(line)) => Self::append_output_line(&mut file, line).await,
                        _ => stdout_done = true,
                    }
                }
                line = async {
                    match stderr_lines.as_mut() {
                        Some(r) => r.next_line().await,
                        None => std::future::pending().await,
                    }
                }, if !stderr_done => {
                    match line {
                        Ok(Some(line)) => Self::append_output_line(&mut file, format!("[stderr] {}", line)).await,
                        _ => stderr_done = true,
                    }
                }
            }
        }

        let status = child.wait().await?;
        let exit_code = status.code();
        Self::append_output_line(
            &mut file,
            format!(
                "--- Command finished with exit code: {} ---",
                exit_code.unwrap_or(-1)
            ),
        )
        .await;

        if status.success() {
            Ok(TaskResult {
                exit_code,
                error: None,
            })
        } else {
            Ok(TaskResult {
                exit_code,
                error: Some(format!(
                    "Command exited with code {}",
                    exit_code.unwrap_or(-1)
                )),
            })
        }
    }

    async fn run_test_build(output_path: PathBuf, reason: &str) -> Result<TaskResult> {
        let mut file = tokio::fs::File::create(&output_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create output file: {}", e))?;
        Self::append_output_line(
            &mut file,
            format!("[test mode] Simulated selfdev build for reason: {}", reason),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Self::append_output_line(&mut file, "--- Command finished with exit code: 0 ---").await;
        Ok(TaskResult {
            exit_code: Some(0),
            error: None,
        })
    }

    async fn run_build_request(
        request_id: String,
        repo_dir: PathBuf,
        command: SelfDevBuildCommand,
        reason: String,
        output_path: PathBuf,
    ) -> Result<TaskResult> {
        let mut request = BuildRequest::load(&request_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing queued build request {}", request_id))?;
        let mut queue_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open output file: {}", e))?;

        let _lock = Self::wait_for_turn(&request_id, &mut queue_file).await?;
        request.state = BuildRequestState::Building;
        request.started_at = Some(Utc::now().to_rfc3339());
        request.version = if Self::is_test_session() {
            Some("test-build".to_string())
        } else {
            build::current_git_hash(&repo_dir).ok()
        };
        request.save()?;
        Self::append_output_line(&mut queue_file, format!("Build starting now: {}", reason)).await;
        drop(queue_file);

        let result = if Self::is_test_session() {
            Self::run_test_build(output_path.clone(), &reason).await?
        } else {
            let result =
                Self::stream_build_command(repo_dir.clone(), command.clone(), output_path.clone())
                    .await?;
            if result.error.is_none() {
                build::publish_local_current_build(&repo_dir)?;
                let mut manifest = build::BuildManifest::load()?;
                manifest.add_to_history(build::current_build_info(&repo_dir)?)?;
            }
            result
        };

        let mut request = BuildRequest::load(&request_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing queued build request {}", request_id))?;
        request.completed_at = Some(Utc::now().to_rfc3339());
        request.state = if result.error.is_some() {
            BuildRequestState::Failed
        } else {
            BuildRequestState::Completed
        };
        request.error = result.error.clone();
        request.save()?;
        Ok(result)
    }

    async fn send_prompt_to_session(session_id: &str, prompt: &str) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut last_error: Option<String> = None;

        while std::time::Instant::now() < deadline {
            match Self::try_send_prompt_once(session_id, prompt).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    last_error = Some(err.to_string());
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }

        Err(anyhow::anyhow!(
            "Timed out delivering prompt to spawned self-dev session {}: {}",
            session_id,
            last_error.unwrap_or_else(|| "unknown error".to_string())
        ))
    }

    async fn try_send_prompt_once(session_id: &str, prompt: &str) -> Result<()> {
        let mut client = server::Client::connect_debug().await?;
        let request_id = client
            .send_transcript(prompt, TranscriptMode::Send, Some(session_id.to_string()))
            .await?;

        loop {
            match client.read_event().await? {
                ServerEvent::Ack { id } if id == request_id => {}
                ServerEvent::Done { id } if id == request_id => return Ok(()),
                ServerEvent::Error { id, message, .. } if id == request_id => {
                    anyhow::bail!(message)
                }
                _ => {}
            }
        }
    }

    async fn do_build(
        &self,
        reason: Option<String>,
        notify: Option<bool>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput> {
        let reason = reason
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "`selfdev build` requires a non-empty `reason` so other queued agents can see why the build is needed."
                )
            })?;
        let repo_dir =
            SelfDevTool::resolve_repo_dir(ctx.working_dir.as_deref()).ok_or_else(|| {
                anyhow::anyhow!("Could not find the jcode repository directory for selfdev build")
            })?;

        let blocker = SelfDevTool::newest_active_request()?;
        let command = SelfDevTool::build_command(&repo_dir);
        let (session_short_name, session_title) = SelfDevTool::load_session_labels(&ctx.session_id);
        let request_id = SelfDevTool::next_request_id();

        let mut request = BuildRequest {
            request_id: request_id.clone(),
            background_task_id: None,
            session_id: ctx.session_id.clone(),
            session_short_name,
            session_title,
            reason: reason.clone(),
            repo_dir: repo_dir.display().to_string(),
            command: command.display.clone(),
            requested_at: Utc::now().to_rfc3339(),
            started_at: None,
            completed_at: None,
            state: BuildRequestState::Queued,
            version: None,
            error: None,
            output_file: None,
            status_file: None,
        };
        request.save()?;

        let request_id_for_task = request_id.clone();
        let repo_dir_for_task = repo_dir.clone();
        let command_for_task = command.clone();
        let reason_for_task = reason.clone();
        let notify = notify.unwrap_or(true);
        let info = background::global()
            .spawn_with_notify(
                "selfdev-build",
                &ctx.session_id,
                notify,
                move |output_path| async move {
                    SelfDevTool::run_build_request(
                        request_id_for_task,
                        repo_dir_for_task,
                        command_for_task,
                        reason_for_task,
                        output_path,
                    )
                    .await
                },
            )
            .await;

        request.background_task_id = Some(info.task_id.clone());
        request.output_file = Some(info.output_file.display().to_string());
        request.status_file = Some(info.status_file.display().to_string());
        request.save()?;

        let queue_position = SelfDevTool::current_queue_position(&request_id)?.unwrap_or(1);
        let mut output = format!(
            "Self-dev build queued in background.\n\n- Request ID: `{}`\n- Task ID: `{}`\n- Reason: {}\n- Command: `{}`\n- Queue position: {}\n- Output file: `{}`\n- Status file: `{}`\n\nYou will{} be notified when the build completes.",
            request_id,
            info.task_id,
            reason,
            command.display,
            queue_position,
            info.output_file.display(),
            info.status_file.display(),
            if notify { "" } else { " not" }
        );

        if let Some(ref blocker) = blocker {
            output.push_str(&format!(
                "\n\nCurrently blocked by: {}\nReason: {}",
                blocker.display_owner(),
                blocker.reason
            ));
        }

        output.push_str(&format!(
            "\n\nUse `bg action=\"status\" task_id=\"{}\"` to check progress, or `selfdev status` to inspect the build queue.\nAfter it finishes, use `selfdev reload` when you want to restart onto the new binary.",
            info.task_id
        ));

        Ok(ToolOutput::new(output).with_metadata(json!({
            "background": true,
            "request_id": request_id,
            "task_id": info.task_id,
            "output_file": info.output_file.to_string_lossy(),
            "status_file": info.status_file.to_string_lossy(),
            "queue_position": queue_position,
            "blocked_by": blocker.as_ref().map(|request| json!({
                "session_id": request.session_id,
                "session_short_name": request.session_short_name,
                "session_title": request.session_title,
                "reason": request.reason,
            }))
        })))
    }

    async fn do_enter(&self, prompt: Option<String>, ctx: &ToolContext) -> Result<ToolOutput> {
        let launch = enter_selfdev_session(ctx.working_dir.as_deref())?;

        if launch.test_mode {
            let mut output = format!(
                "Created self-dev session {} in {}.\n\nTest mode skipped launching a new terminal.",
                launch.session_id,
                launch.repo_dir.display()
            );
            if let Some(prompt) = prompt {
                output.push_str(&format!(
                    "\n\nSeed prompt captured ({} chars) but not delivered in test mode.",
                    prompt.chars().count()
                ));
            }
            return Ok(ToolOutput::new(output).with_metadata(json!({
                "session_id": launch.session_id,
                "repo_dir": launch.repo_dir,
                "launched": false,
                "test_mode": true
            })));
        }

        if !launch.launched {
            let command_preview = launch
                .command_preview()
                .unwrap_or_else(|| format!("jcode --resume {} self-dev", launch.session_id));
            return Ok(ToolOutput::new(format!(
                "Created self-dev session {} but could not find a supported terminal to spawn automatically.\n\nRun manually:\n`{} --resume {} self-dev`",
                launch.session_id,
                launch.exe.as_ref().map(|exe| exe.display().to_string()).unwrap_or_else(|| "jcode".to_string()),
                launch.session_id
            ))
            .with_metadata(json!({
                "session_id": launch.session_id,
                "repo_dir": launch.repo_dir,
                "launched": false
            }))
            .with_title(format!("selfdev enter: {}", command_preview)));
        }

        let mut output = format!(
            "Spawned a new self-dev session in a separate terminal.\n\n- Session: `{}`\n- Repo: `{}`\n- Command: `{} --resume {} self-dev`",
            launch.session_id,
            launch.repo_dir.display(),
            launch
                .exe
                .as_ref()
                .map(|exe| exe.display().to_string())
                .unwrap_or_else(|| "jcode".to_string()),
            launch.session_id
        );

        let prompt_delivery = if let Some(prompt_text) = prompt {
            match SelfDevTool::send_prompt_to_session(&launch.session_id, &prompt_text).await {
                Ok(()) => {
                    output.push_str("\n- Prompt: delivered to the spawned self-dev session");
                    Some(true)
                }
                Err(err) => {
                    output.push_str(&format!("\n- Prompt: failed to auto-deliver ({})", err));
                    Some(false)
                }
            }
        } else {
            None
        };

        Ok(ToolOutput::new(output).with_metadata(json!({
            "session_id": launch.session_id,
            "repo_dir": launch.repo_dir,
            "launched": true,
            "prompt_delivered": prompt_delivery
        })))
    }

    async fn do_reload(
        &self,
        context: Option<String>,
        session_id: &str,
        execution_mode: ToolExecutionMode,
    ) -> Result<ToolOutput> {
        let repo_dir = build::get_repo_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find jcode repository directory"))?;

        let target_binary = build::find_dev_binary(&repo_dir)
            .unwrap_or_else(|| build::release_binary_path(&repo_dir));
        if !target_binary.exists() {
            return Ok(ToolOutput::new(
                format!(
                    "No binary found at {}.\n\
                     Run 'jcode self-dev --build' first, or build with 'scripts/dev_cargo.sh build --release --bin jcode' and then try reload again.",
                    target_binary.display()
                )
                .to_string(),
            ));
        }

        let hash = if SelfDevTool::is_test_session() {
            "test-reload-hash".to_string()
        } else {
            build::current_git_hash(&repo_dir)?
        };
        let version_before = env!("JCODE_VERSION").to_string();

        // Publish the newly built binary as the active current build so reload,
        // launcher sessions, and self-dev spawns all converge on the same binary.
        if !SelfDevTool::is_test_session() {
            build::publish_local_current_build(&repo_dir)?;
            build::install_version(&repo_dir, &hash)?;
            build::update_canary_symlink(&hash)?;
        }

        // Update manifest - track what we're testing
        let mut manifest = build::BuildManifest::load()?;
        manifest.canary = Some(hash.clone());
        manifest.canary_status = Some(build::CanaryStatus::Testing);
        manifest.save()?;

        // Save reload context for continuation after restart
        let reload_ctx = ReloadContext {
            task_context: context,
            version_before,
            version_after: hash.clone(),
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        crate::logging::info(&format!(
            "Saving reload context to {:?}",
            ReloadContext::path_for_session(session_id)
        ));
        if let Err(e) = reload_ctx.save() {
            crate::logging::error(&format!("Failed to save reload context: {}", e));
            return Err(e);
        }
        crate::logging::info("Reload context saved successfully");

        // Write reload info for post-restart display
        let info_path = crate::storage::jcode_dir()?.join("reload-info");
        let info = format!("reload:{}", hash);
        std::fs::write(&info_path, &info)?;

        // Signal the server via in-process channel (replaces filesystem-based rebuild-signal)
        let request_id =
            server::send_reload_signal(hash.clone(), Some(session_id.to_string()), true);
        crate::logging::info(&format!(
            "selfdev reload: request={} session_id={} hash={} execution_mode={:?}",
            request_id, session_id, hash, execution_mode
        ));

        let timeout = std::time::Duration::from_secs(SelfDevTool::reload_timeout_secs());
        let ack_wait_started = std::time::Instant::now();
        let ack = server::wait_for_reload_ack(&request_id, timeout)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Timed out waiting for the server to begin reload after {}s: {}. The reload signal may not have been picked up; check that the connected server is running a build with unified self-dev reload support and try restarting the shared server.",
                    timeout.as_secs(),
                    error
                )
            })?;

        crate::logging::info(&format!(
            "selfdev reload: acked request={} hash={} after {}ms state={}",
            ack.request_id,
            ack.hash,
            ack_wait_started.elapsed().as_millis(),
            server::reload_state_summary(std::time::Duration::from_secs(60))
        ));

        match execution_mode {
            ToolExecutionMode::Direct => Ok(ToolOutput::new(format!(
                "Reload acknowledged for build {}. Server is restarting now.",
                ack.hash
            ))),
            ToolExecutionMode::AgentTurn => {
                let sleep_forever = async {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    }
                };

                match tokio::time::timeout(timeout, sleep_forever).await {
                    Ok(_) => unreachable!("infinite wait future unexpectedly completed"),
                    Err(_) => {
                        crate::logging::warn(&format!(
                            "selfdev reload: request={} not interrupted after {}ms state={} ",
                            ack.request_id,
                            timeout.as_millis(),
                            server::reload_state_summary(std::time::Duration::from_secs(60))
                        ));
                        Err(anyhow::anyhow!(
                            "Reload was acknowledged by the server for build {}, but this tool execution was not interrupted within {}s. The server restart may be stuck; inspect logs and active sessions. Current reload state: {}",
                            ack.hash,
                            timeout.as_secs(),
                            server::reload_state_summary(std::time::Duration::from_secs(60))
                        ))
                    }
                }
            }
        }
    }

    async fn do_status(&self) -> Result<ToolOutput> {
        selfdev_status_output()
    }

    async fn do_socket_info(&self) -> Result<ToolOutput> {
        let debug_socket = server::debug_socket_path();
        let main_socket = server::socket_path();

        let info = json!({
            "debug_socket": debug_socket.to_string_lossy(),
            "main_socket": main_socket.to_string_lossy(),
            "debug_enabled": crate::config::config().display.debug_socket ||
                             std::env::var("JCODE_DEBUG_CONTROL").is_ok() ||
                             crate::storage::jcode_dir().map(|d| d.join("debug_control").exists()).unwrap_or(false),
            "connect_example": format!(
                "echo '{{\"type\":\"debug_command\",\"id\":1,\"command\":\"help\"}}' | nc -U {}",
                debug_socket.display()
            ),
        });

        Ok(ToolOutput::new(format!(
            "## Debug Socket Info\n\n\
             **Debug socket:** {}\n\
             **Main socket:** {}\n\n\
             Use the `debug_socket` tool to send commands, or connect directly:\n\
             ```bash\n\
             echo '{{\"type\":\"debug_command\",\"id\":1,\"command\":\"help\"}}' | nc -U {}\n\
             ```\n\n\
             For programmatic access, use the `debug_socket` tool with the command parameter.",
            debug_socket.display(),
            main_socket.display(),
            debug_socket.display()
        ))
        .with_metadata(info))
    }

    async fn do_socket_help(&self) -> Result<ToolOutput> {
        Ok(ToolOutput::new(
            r#"## Debug Socket Commands

Commands are namespaced with `server:`, `client:`, or `tester:` prefixes.
Unnamespaced commands default to `server:`.

### Server Commands (agent/tools)
| Command | Description |
|---------|-------------|
| `state` | Agent state (session, model, canary) |
| `history` | Conversation history as JSON |
| `tools` | List available tools |
| `last_response` | Last assistant response |
| `message:<text>` | Send message, get LLM response |
| `tool:<name> <json>` | Execute tool directly |
| `sessions` | List all sessions |
| `create_session` | Create headless session |
| `help` | Full help text |

### Client Commands (TUI/visual debug)
| Command | Description |
|---------|-------------|
| `client:frame` | Get latest visual debug frame (JSON) |
| `client:frame-normalized` | Normalized frame for diffs |
| `client:screen` | Dump frames to file |
| `client:enable` | Enable visual debug capture |
| `client:disable` | Disable visual debug capture |
| `client:status` | Client debug status |
| `client:scroll-test[:<json>]` | Run offscreen scroll+diagram test |
| `client:scroll-suite[:<json>]` | Run scroll+diagram test suite |

### Tester Commands (spawn test instances)
| Command | Description |
|---------|-------------|
| `tester:spawn` | Spawn new tester instance |
| `tester:spawn {"cwd":"/path"}` | Spawn with options |
| `tester:list` | List active testers |
| `tester:<id>:frame` | Get frame from tester |
| `tester:<id>:state` | Get tester state |
| `tester:<id>:message:<text>` | Send message to tester |
| `tester:<id>:scroll-test[:<json>]` | Run offscreen scroll+diagram test |
| `tester:<id>:scroll-suite[:<json>]` | Run scroll+diagram test suite |
| `tester:<id>:stop` | Stop tester |

Use the `debug_socket` tool to execute these commands directly."#
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BackgroundTaskStatus;
    use std::ffi::OsStr;
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let original = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            crate::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => crate::env::set_var(self.key, value),
                None => crate::env::remove_var(self.key),
            }
        }
    }

    fn create_test_context(
        session_id: &str,
        working_dir: Option<std::path::PathBuf>,
    ) -> ToolContext {
        ToolContext {
            session_id: session_id.to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-tool-call".to_string(),
            working_dir,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    fn create_repo_fixture() -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().expect("temp repo");
        std::fs::create_dir_all(temp.path().join(".git")).expect("git dir");
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"jcode\"\nversion = \"0.1.0\"\n",
        )
        .expect("cargo toml");
        temp
    }

    async fn wait_for_task_completion(task_id: &str) -> background::TaskStatusFile {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = background::global().status(task_id).await {
                if status.status != BackgroundTaskStatus::Running {
                    return status;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for background task {}",
                task_id
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn test_reload_context_serialization() {
        // Create test context with task info
        let ctx = ReloadContext {
            task_context: Some("Testing the reload feature".to_string()),
            version_before: "v0.1.100".to_string(),
            version_after: "abc1234".to_string(),
            session_id: "test-session-123".to_string(),
            timestamp: "2025-01-20T00:00:00Z".to_string(),
        };

        // Serialize and deserialize
        let json = serde_json::to_string(&ctx).unwrap();
        let loaded: ReloadContext = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded.task_context,
            Some("Testing the reload feature".to_string())
        );
        assert_eq!(loaded.version_before, "v0.1.100");
        assert_eq!(loaded.version_after, "abc1234");
        assert_eq!(loaded.session_id, "test-session-123");
    }

    #[test]
    fn test_reload_context_path() {
        // Just verify the session-scoped path function works
        let path = ReloadContext::path_for_session("test-session-123");
        assert!(path.is_ok());
        let path = path.unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("reload-context-test-session-123.json"));
    }

    #[test]
    fn test_reload_context_save_and_load_for_session_uses_session_scoped_file() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let temp_home = tempfile::TempDir::new().expect("temp home");
        let _home_guard = EnvVarGuard::set("JCODE_HOME", temp_home.path());

        let ctx = ReloadContext {
            task_context: Some("Testing scoped reload context".to_string()),
            version_before: "v0.1.100".to_string(),
            version_after: "abc1234".to_string(),
            session_id: "test-session-123".to_string(),
            timestamp: "2025-01-20T00:00:00Z".to_string(),
        };

        ctx.save().expect("save reload context");

        let path = ReloadContext::path_for_session("test-session-123").expect("context path");
        assert!(
            path.exists(),
            "session-scoped reload context file should exist"
        );

        let peeked = ReloadContext::peek_for_session("test-session-123")
            .expect("peek should succeed")
            .expect("context should exist");
        assert_eq!(peeked.session_id, "test-session-123");

        let loaded = ReloadContext::load_for_session("test-session-123")
            .expect("load should succeed")
            .expect("context should exist");
        assert_eq!(loaded.session_id, "test-session-123");
        assert!(
            !path.exists(),
            "load_for_session should consume the context file"
        );
    }

    #[test]
    fn reload_timeout_secs_defaults_to_15() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let _guard = EnvVarGuard::remove("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS");
        assert_eq!(SelfDevTool::reload_timeout_secs(), 15);
    }

    #[test]
    fn reload_timeout_secs_honors_valid_env_override() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let _guard = EnvVarGuard::set("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS", "27");
        assert_eq!(SelfDevTool::reload_timeout_secs(), 27);
    }

    #[test]
    fn reload_timeout_secs_ignores_empty_invalid_and_zero_values() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let _guard = EnvVarGuard::set("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS", "   ");
        assert_eq!(SelfDevTool::reload_timeout_secs(), 15);
        drop(_guard);

        let _guard = EnvVarGuard::set("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS", "abc");
        assert_eq!(SelfDevTool::reload_timeout_secs(), 15);
        drop(_guard);

        let _guard = EnvVarGuard::set("JCODE_SELFDEV_RELOAD_TIMEOUT_SECS", "0");
        assert_eq!(SelfDevTool::reload_timeout_secs(), 15);
    }

    #[tokio::test]
    async fn do_reload_returns_after_ack_in_direct_mode() {
        let request_id = server::send_reload_signal("direct-hash".to_string(), None, true);
        let waiter = tokio::spawn({
            let request_id = request_id.clone();
            async move {
                server::wait_for_reload_ack(&request_id, std::time::Duration::from_secs(1)).await
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        server::acknowledge_reload_signal(&crate::server::ReloadSignal {
            hash: "direct-hash".to_string(),
            triggering_session: None,
            prefer_selfdev_binary: true,
            request_id: "ignored".to_string(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        server::acknowledge_reload_signal(&crate::server::ReloadSignal {
            hash: "direct-hash".to_string(),
            triggering_session: None,
            prefer_selfdev_binary: true,
            request_id,
        });

        let ack = waiter
            .await
            .expect("waiter task should complete")
            .expect("ack should be received");
        assert_eq!(ack.hash, "direct-hash");
    }

    #[tokio::test]
    async fn enter_creates_selfdev_session_in_test_mode() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let temp_home = tempfile::TempDir::new().expect("temp home");
        let _home_guard = EnvVarGuard::set("JCODE_HOME", temp_home.path());
        let _test_guard = EnvVarGuard::set("JCODE_TEST_SESSION", "1");
        let repo = create_repo_fixture();

        let tool = SelfDevTool::new();
        let ctx = create_test_context("origin-session", Some(repo.path().to_path_buf()));
        let output = tool
            .execute(
                json!({"action": "enter", "prompt": "Work on jcode itself"}),
                ctx,
            )
            .await
            .expect("selfdev enter should succeed in test mode");

        assert!(output.output.contains("Created self-dev session"));
        assert!(
            output
                .output
                .contains("Test mode skipped launching a new terminal")
        );

        let metadata = output.metadata.expect("metadata");
        let session_id = metadata["session_id"]
            .as_str()
            .expect("session id metadata");
        let session = session::Session::load(session_id).expect("load spawned session");
        assert!(
            session.is_canary,
            "spawned session should be canary/self-dev"
        );
        assert_eq!(session.testing_build.as_deref(), Some("self-dev"));
        assert_eq!(
            session.working_dir.as_deref(),
            Some(repo.path().to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn reload_requires_selfdev_session() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let temp_home = tempfile::TempDir::new().expect("temp home");
        let _home_guard = EnvVarGuard::set("JCODE_HOME", temp_home.path());

        let mut session = session::Session::create(None, Some("Normal Session".to_string()));
        session.save().expect("save session");

        let tool = SelfDevTool::new();
        let ctx = create_test_context(&session.id, session.working_dir.clone().map(Into::into));
        let output = tool
            .execute(json!({"action": "reload"}), ctx)
            .await
            .expect("reload should return guidance instead of failing");

        assert!(
            output
                .output
                .contains("only available inside a self-dev session")
        );
        assert!(output.output.contains("selfdev enter"));
    }

    #[tokio::test]
    async fn build_requires_reason() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let temp_home = tempfile::TempDir::new().expect("temp home");
        let _home_guard = EnvVarGuard::set("JCODE_HOME", temp_home.path());
        let _test_guard = EnvVarGuard::set("JCODE_TEST_SESSION", "1");
        let repo = create_repo_fixture();

        let tool = SelfDevTool::new();
        let ctx = create_test_context("build-session", Some(repo.path().to_path_buf()));
        let err = tool
            .execute(json!({"action": "build"}), ctx)
            .await
            .expect_err("build without reason should fail");

        assert!(err.to_string().contains("requires a non-empty `reason`"));
    }

    #[tokio::test]
    async fn build_queues_background_tasks_and_reports_queue_status() {
        let _storage_guard = crate::storage::lock_test_env();
        let _lock = lock_env();
        let temp_home = tempfile::TempDir::new().expect("temp home");
        let _home_guard = EnvVarGuard::set("JCODE_HOME", temp_home.path());
        let _test_guard = EnvVarGuard::set("JCODE_TEST_SESSION", "1");
        let repo = create_repo_fixture();

        let mut session_one =
            session::Session::create(None, Some("First build session".to_string()));
        session_one.short_name = Some("alpha".to_string());
        session_one.save().expect("save session one");

        let mut session_two =
            session::Session::create(None, Some("Second build session".to_string()));
        session_two.short_name = Some("beta".to_string());
        session_two.save().expect("save session two");

        let tool = SelfDevTool::new();
        let first = tool
            .execute(
                json!({"action": "build", "reason": "first reason"}),
                create_test_context(&session_one.id, Some(repo.path().to_path_buf())),
            )
            .await
            .expect("first build should queue");
        let second = tool
            .execute(
                json!({"action": "build", "reason": "second reason"}),
                create_test_context(&session_two.id, Some(repo.path().to_path_buf())),
            )
            .await
            .expect("second build should queue");

        let first_meta = first.metadata.expect("first metadata");
        let second_meta = second.metadata.expect("second metadata");
        let first_task_id = first_meta["task_id"].as_str().expect("first task id");
        let second_task_id = second_meta["task_id"].as_str().expect("second task id");

        assert_eq!(first_meta["queue_position"].as_u64(), Some(1));
        assert_eq!(second_meta["queue_position"].as_u64(), Some(2));
        assert!(second.output.contains("second reason"));

        let status_output = selfdev_status_output().expect("status output");
        assert!(status_output.output.contains("## Build Queue"));
        assert!(status_output.output.contains("first reason"));
        assert!(status_output.output.contains("second reason"));

        let first_status = wait_for_task_completion(first_task_id).await;
        let second_status = wait_for_task_completion(second_task_id).await;
        assert_eq!(first_status.status, BackgroundTaskStatus::Completed);
        assert_eq!(second_status.status, BackgroundTaskStatus::Completed);

        let request_one =
            BuildRequest::load(first_meta["request_id"].as_str().expect("first request id"))
                .expect("load request one")
                .expect("request one exists");
        let request_two = BuildRequest::load(
            second_meta["request_id"]
                .as_str()
                .expect("second request id"),
        )
        .expect("load request two")
        .expect("request two exists");
        assert_eq!(request_one.state, BuildRequestState::Completed);
        assert_eq!(request_two.state, BuildRequestState::Completed);
    }
}
