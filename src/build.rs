use crate::storage;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

const FNV_OFFSET_BASIS_64: u64 = 0xcbf29ce484222325;
const FNV_PRIME_64: u64 = 0x100000001b3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfDevBuildCommand {
    pub program: String,
    pub args: Vec<String>,
    pub display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceState {
    pub repo_scope: String,
    pub worktree_scope: String,
    pub short_hash: String,
    pub full_hash: String,
    pub dirty: bool,
    pub fingerprint: String,
    pub version_label: String,
    pub changed_paths: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedBuild {
    pub version: String,
    pub source_fingerprint: String,
    pub versioned_path: PathBuf,
    pub current_link: PathBuf,
    pub launcher_link: PathBuf,
    pub previous_current_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingActivation {
    pub session_id: String,
    pub new_version: String,
    pub previous_current_version: Option<String>,
    pub source_fingerprint: Option<String>,
    pub requested_at: DateTime<Utc>,
}

fn stable_hash_update(state: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *state ^= u64::from(*byte);
        *state = state.wrapping_mul(FNV_PRIME_64);
    }
}

fn stable_hash_str(state: &mut u64, value: &str) {
    stable_hash_update(state, value.as_bytes());
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut state = FNV_OFFSET_BASIS_64;
    stable_hash_update(&mut state, bytes);
    format!("{state:016x}")
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn hash_path_scope(path: &Path) -> String {
    stable_hash_hex(canonicalize_or_self(path).to_string_lossy().as_bytes())
}

/// Get the jcode repository directory
pub fn get_repo_dir() -> Option<PathBuf> {
    // First try: compile-time directory
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir);
    if is_jcode_repo(&path) {
        return Some(path);
    }

    // Fallback: check relative to executable
    if let Ok(exe) = std::env::current_exe() {
        // Assume structure: repo/target/<profile>/<binary> (platform-specific executable name)
        if let Some(repo) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            && is_jcode_repo(repo)
        {
            return Some(repo.to_path_buf());
        }
    }

    // Final fallback: search upward from current working directory.
    // This matters for self-dev sessions launched from the repo but running
    // from an installed canary/stable binary whose current_exe() is outside
    // the source tree.
    if let Ok(cwd) = std::env::current_dir()
        && let Some(repo) = find_repo_in_ancestors(&cwd)
    {
        return Some(repo);
    }

    None
}

fn find_repo_in_ancestors(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if is_jcode_repo(dir) {
            return Some(dir.to_path_buf());
        }
    }
    None
}

pub fn binary_stem() -> &'static str {
    "jcode"
}

pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "jcode.exe"
    } else {
        binary_stem()
    }
}

pub const SELFDEV_CARGO_PROFILE: &str = "selfdev";

fn profile_binary_path(repo_dir: &std::path::Path, profile: &str) -> PathBuf {
    repo_dir.join("target").join(profile).join(binary_name())
}

pub fn release_binary_path(repo_dir: &std::path::Path) -> PathBuf {
    profile_binary_path(repo_dir, "release")
}

pub fn selfdev_binary_path(repo_dir: &std::path::Path) -> PathBuf {
    profile_binary_path(repo_dir, SELFDEV_CARGO_PROFILE)
}

fn binary_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn newest_existing_binary(
    candidates: Vec<(PathBuf, &'static str)>,
) -> Option<(PathBuf, &'static str)> {
    candidates
        .into_iter()
        .filter(|(path, _)| path.exists())
        .max_by_key(|(path, _)| binary_mtime(path))
}

pub fn selfdev_build_command(repo_dir: &Path) -> SelfDevBuildCommand {
    let wrapper = repo_dir.join("scripts").join("dev_cargo.sh");
    if wrapper.is_file() {
        return SelfDevBuildCommand {
            program: "bash".to_string(),
            args: vec![
                wrapper.to_string_lossy().into_owned(),
                "build".to_string(),
                "--profile".to_string(),
                SELFDEV_CARGO_PROFILE.to_string(),
                "-p".to_string(),
                "jcode".to_string(),
                "--bin".to_string(),
                "jcode".to_string(),
            ],
            display: format!(
                "scripts/dev_cargo.sh build --profile {} -p jcode --bin jcode",
                SELFDEV_CARGO_PROFILE
            ),
        };
    }

    SelfDevBuildCommand {
        program: "cargo".to_string(),
        args: vec![
            "build".to_string(),
            "--profile".to_string(),
            SELFDEV_CARGO_PROFILE.to_string(),
            "-p".to_string(),
            "jcode".to_string(),
            "--bin".to_string(),
            "jcode".to_string(),
        ],
        display: format!(
            "cargo build --profile {} -p jcode --bin jcode",
            SELFDEV_CARGO_PROFILE
        ),
    }
}

pub fn run_selfdev_build(repo_dir: &Path) -> Result<SelfDevBuildCommand> {
    let build = selfdev_build_command(repo_dir);
    let status = Command::new(&build.program)
        .args(&build.args)
        .current_dir(repo_dir)
        .status()?;

    if !status.success() {
        anyhow::bail!("Build failed: {}", build.display);
    }

    Ok(build)
}

pub fn current_binary_built_at() -> Option<DateTime<Utc>> {
    let modified: SystemTime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())?;
    Some(DateTime::<Utc>::from(modified))
}

pub fn current_binary_build_time_string() -> Option<String> {
    current_binary_built_at().map(|dt| dt.format("%Y-%m-%d %H:%M:%S %z").to_string())
}

/// Find the best development binary in the repo.
/// Prefers the newest local self-dev or release binary.
pub fn find_dev_binary(repo_dir: &std::path::Path) -> Option<PathBuf> {
    newest_existing_binary(vec![
        (selfdev_binary_path(repo_dir), "repo-selfdev"),
        (release_binary_path(repo_dir), "repo-release"),
    ])
    .map(|(path, _)| path)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("USERPROFILE").map(PathBuf::from))
        .map_err(|_| anyhow::anyhow!("HOME/USERPROFILE not set"))
}

/// Directory for the single launcher path users execute from PATH.
///
/// Defaults to `~/.local/bin` on Unix, `%LOCALAPPDATA%\jcode\bin` on Windows.
/// Overridable with `JCODE_INSTALL_DIR`.
pub fn launcher_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("JCODE_INSTALL_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Ok(sandbox_home) = std::env::var("JCODE_HOME") {
        let trimmed = sandbox_home.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed).join("bin"));
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("jcode").join("bin"));
        }
        Ok(home_dir()?
            .join("AppData")
            .join("Local")
            .join("jcode")
            .join("bin"))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir()?.join(".local").join("bin"))
    }
}

/// Path to the launcher binary (`~/.local/bin/jcode` by default).
pub fn launcher_binary_path() -> Result<PathBuf> {
    Ok(launcher_dir()?.join(binary_name()))
}

fn update_launcher_symlink(target: &Path) -> Result<PathBuf> {
    let launcher = launcher_binary_path()?;

    if let Some(parent) = launcher.parent() {
        storage::ensure_dir(parent)?;
    }

    let temp = launcher
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{}-launcher-{}",
            binary_stem(),
            std::process::id()
        ));

    crate::platform::atomic_symlink_swap(target, &launcher, &temp)?;
    Ok(launcher)
}

/// Update launcher path to point at the current channel binary.
pub fn update_launcher_symlink_to_current() -> Result<PathBuf> {
    let current = current_binary_path()?;
    update_launcher_symlink(&current)
}

/// Update launcher path to point at the stable channel binary.
pub fn update_launcher_symlink_to_stable() -> Result<PathBuf> {
    let stable = stable_binary_path()?;
    update_launcher_symlink(&stable)
}

/// Resolve which client binary should be considered for launches, updates, and reloads.
///
/// Order matters:
/// - Prefer the published `current` channel first (active local build)
/// - Self-dev sessions can fall back to an unpublished repo build from `target/selfdev` or `target/release`
/// - Then the self-dev canary channel
/// - Then launcher path
/// - Then stable channel path
/// - Finally currently running executable
pub fn client_update_candidate(is_selfdev_session: bool) -> Option<(PathBuf, &'static str)> {
    if let Ok(current) = current_binary_path()
        && current.exists()
    {
        return Some((current, "current"));
    }

    if is_selfdev_session {
        if let Some(repo_dir) = get_repo_dir()
            && let Some(dev) = find_dev_binary(&repo_dir)
            && dev.exists()
        {
            return Some((dev, "dev"));
        }
        if let Ok(canary) = canary_binary_path()
            && canary.exists()
        {
            return Some((canary, "canary"));
        }
    }

    if let Ok(launcher) = launcher_binary_path()
        && launcher.exists()
    {
        return Some((launcher, "launcher"));
    }

    if let Ok(stable) = stable_binary_path()
        && stable.exists()
    {
        return Some((stable, "stable"));
    }

    std::env::current_exe().ok().map(|exe| (exe, "current"))
}

/// Resolve the best binary to use for `/reload`.
///
/// This mostly follows `client_update_candidate`, but if a freshly built repo
/// release binary exists and is newer than the selected channel binary, prefer
/// that so local rebuilds can reload correctly even if publishing the build
/// failed.
pub fn preferred_reload_candidate(is_selfdev_session: bool) -> Option<(PathBuf, &'static str)> {
    let candidate = client_update_candidate(is_selfdev_session);

    let repo_binary = get_repo_dir().and_then(|repo_dir| {
        if is_selfdev_session {
            newest_existing_binary(vec![
                (selfdev_binary_path(&repo_dir), "repo-selfdev"),
                (release_binary_path(&repo_dir), "repo-release"),
            ])
        } else {
            newest_existing_binary(vec![(release_binary_path(&repo_dir), "repo-release")])
        }
    });

    let repo_is_newer = |repo: &Path, current: &Path| {
        let repo_mtime = std::fs::metadata(repo).ok().and_then(|m| m.modified().ok());
        let current_mtime = std::fs::metadata(current)
            .ok()
            .and_then(|m| m.modified().ok());
        match (repo_mtime, current_mtime) {
            (Some(repo), Some(current)) => repo > current,
            (Some(_), None) => true,
            _ => false,
        }
    };

    match (repo_binary, candidate) {
        (Some((repo, label)), Some((current, _))) if repo_is_newer(&repo, &current) => {
            Some((repo, label))
        }
        (Some((repo, label)), None) => Some((repo, label)),
        (_, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

/// Check if a directory is the jcode repository
pub fn is_jcode_repo(dir: &std::path::Path) -> bool {
    // Check for Cargo.toml with name = "jcode"
    let cargo_toml = dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        return false;
    }

    // Check for .git directory
    if !dir.join(".git").exists() {
        return false;
    }

    // Read Cargo.toml and check package name
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        // Simple check - look for 'name = "jcode"' in [package] section
        if content.contains("name = \"jcode\"") {
            return true;
        }
    }

    false
}

/// Status of a canary build being tested
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CanaryStatus {
    /// Build is currently being tested
    #[serde(alias = "Testing")]
    Testing,
    /// Build passed all tests and is ready for promotion
    #[serde(alias = "Passed")]
    Passed,
    /// Build failed testing
    #[serde(alias = "Failed")]
    Failed,
}

/// Information about a specific build version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Git commit hash (short)
    pub hash: String,
    /// Git commit hash (full)
    pub full_hash: String,
    /// Build timestamp
    pub built_at: DateTime<Utc>,
    /// Git commit message (first line)
    pub commit_message: Option<String>,
    /// Whether build is from dirty working tree
    pub dirty: bool,
    /// Stable fingerprint of the source state used to produce the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    /// Immutable published version label, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_label: Option<String>,
}

/// Manifest tracking build versions and their status
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildManifest {
    /// Current stable build hash (known good)
    pub stable: Option<String>,
    /// Current canary build hash (being tested)
    pub canary: Option<String>,
    /// Session ID testing the canary build
    pub canary_session: Option<String>,
    /// Status of canary testing
    pub canary_status: Option<CanaryStatus>,
    /// History of recent builds
    #[serde(default)]
    pub history: Vec<BuildInfo>,
    /// Last crash information (if canary crashed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_crash: Option<CrashInfo>,
    /// Pending activation being validated across reload/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_activation: Option<PendingActivation>,
}

/// Information about a crash during canary testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashInfo {
    /// Build hash that crashed
    pub build_hash: String,
    /// Exit code
    pub exit_code: i32,
    /// Stderr output (truncated)
    pub stderr: String,
    /// Timestamp of crash
    pub crashed_at: DateTime<Utc>,
    /// Git diff that was being tested
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// Context saved before migrating to a canary build
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationContext {
    pub session_id: String,
    pub from_version: String,
    pub to_version: String,
    pub change_summary: Option<String>,
    pub diff: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl BuildManifest {
    /// Load manifest from disk
    pub fn load() -> Result<Self> {
        let path = manifest_path()?;
        if path.exists() {
            storage::read_json(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Save manifest to disk
    pub fn save(&self) -> Result<()> {
        let path = manifest_path()?;
        storage::write_json(&path, self)
    }

    /// Check if we should use stable or canary for a given session
    pub fn binary_for_session(&self, session_id: &str) -> BinaryChoice {
        // If this session is the canary tester, use canary
        if let Some(ref canary_session) = self.canary_session
            && canary_session == session_id
            && let Some(ref canary) = self.canary
        {
            return BinaryChoice::Canary(canary.clone());
        }
        // Otherwise use stable
        if let Some(ref stable) = self.stable {
            BinaryChoice::Stable(stable.clone())
        } else {
            BinaryChoice::Current
        }
    }

    /// Start canary testing for a session
    pub fn start_canary(&mut self, hash: &str, session_id: &str) -> Result<()> {
        self.canary = Some(hash.to_string());
        self.canary_session = Some(session_id.to_string());
        self.canary_status = Some(CanaryStatus::Testing);
        self.save()
    }

    /// Mark canary as passed
    pub fn mark_canary_passed(&mut self) -> Result<()> {
        self.canary_status = Some(CanaryStatus::Passed);
        self.save()
    }

    /// Mark canary as failed
    pub fn mark_canary_failed(&mut self) -> Result<()> {
        self.canary_status = Some(CanaryStatus::Failed);
        self.save()
    }

    /// Record a crash
    pub fn record_crash(
        &mut self,
        hash: &str,
        exit_code: i32,
        stderr: &str,
        diff: Option<String>,
    ) -> Result<()> {
        self.last_crash = Some(CrashInfo {
            build_hash: hash.to_string(),
            exit_code,
            stderr: stderr.chars().take(4096).collect(), // Truncate
            crashed_at: Utc::now(),
            diff,
        });
        self.canary_status = Some(CanaryStatus::Failed);
        self.save()
    }

    /// Clear crash info after it's been handled
    pub fn clear_crash(&mut self) -> Result<()> {
        self.last_crash = None;
        self.save()
    }

    pub fn set_pending_activation(&mut self, activation: PendingActivation) -> Result<()> {
        self.pending_activation = Some(activation);
        self.save()
    }

    pub fn clear_pending_activation(&mut self) -> Result<()> {
        self.pending_activation = None;
        self.save()
    }

    /// Add build to history
    pub fn add_to_history(&mut self, info: BuildInfo) -> Result<()> {
        // Keep last 20 builds
        self.history.insert(0, info);
        self.history.truncate(20);
        self.save()
    }
}

pub fn complete_pending_activation_for_session(session_id: &str) -> Result<Option<String>> {
    let mut manifest = BuildManifest::load()?;
    let Some(pending) = manifest.pending_activation.clone() else {
        return Ok(None);
    };
    if pending.session_id != session_id {
        return Ok(None);
    }

    manifest.canary = Some(pending.new_version.clone());
    manifest.canary_session = Some(session_id.to_string());
    manifest.canary_status = Some(CanaryStatus::Passed);
    manifest.pending_activation = None;
    manifest.last_crash = None;
    manifest.save()?;
    Ok(Some(pending.new_version))
}

pub fn rollback_pending_activation_for_session(session_id: &str) -> Result<Option<String>> {
    let mut manifest = BuildManifest::load()?;
    let Some(pending) = manifest.pending_activation.clone() else {
        return Ok(None);
    };
    if pending.session_id != session_id {
        return Ok(None);
    }

    if let Some(previous) = pending.previous_current_version.as_deref() {
        update_current_symlink(previous)?;
        update_launcher_symlink_to_current()?;
    }
    manifest.canary_status = Some(CanaryStatus::Failed);
    manifest.pending_activation = None;
    manifest.save()?;
    Ok(Some(pending.new_version))
}

/// Which binary to use
#[derive(Debug, Clone)]
pub enum BinaryChoice {
    /// Use the stable version
    Stable(String),
    /// Use the canary version (for testing)
    Canary(String),
    /// Use current running binary (no versioned builds yet)
    Current,
}

/// Get path to builds directory
pub fn builds_dir() -> Result<PathBuf> {
    let base = storage::jcode_dir()?;
    let dir = base.join("builds");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

/// Get path to build manifest
pub fn manifest_path() -> Result<PathBuf> {
    Ok(builds_dir()?.join("manifest.json"))
}

/// Get path to a specific version's binary
pub fn version_binary_path(hash: &str) -> Result<PathBuf> {
    Ok(builds_dir()?
        .join("versions")
        .join(hash)
        .join(binary_name()))
}

/// Get path to stable symlink
pub fn stable_binary_path() -> Result<PathBuf> {
    Ok(builds_dir()?.join("stable").join(binary_name()))
}

/// Get path to current symlink (active local build channel)
pub fn current_binary_path() -> Result<PathBuf> {
    Ok(builds_dir()?.join("current").join(binary_name()))
}

/// Get path to canary binary
pub fn canary_binary_path() -> Result<PathBuf> {
    Ok(builds_dir()?.join("canary").join(binary_name()))
}

/// Get path to migration context file
pub fn migration_context_path(session_id: &str) -> Result<PathBuf> {
    Ok(builds_dir()?
        .join("migrations")
        .join(format!("{}.json", session_id)))
}

/// Get path to stable version file (watched by other sessions)
pub fn stable_version_file() -> Result<PathBuf> {
    Ok(builds_dir()?.join("stable-version"))
}

/// Get path to current version file (active local build marker).
pub fn current_version_file() -> Result<PathBuf> {
    Ok(builds_dir()?.join("current-version"))
}

fn git_output_bytes(repo_dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed with status {:?}",
            args.join(" "),
            output.status.code()
        );
    }
    Ok(output.stdout)
}

fn git_common_dir(repo_dir: &Path) -> Result<PathBuf> {
    let output = git_output_bytes(repo_dir, &["rev-parse", "--git-common-dir"])?;
    let raw = String::from_utf8_lossy(&output).trim().to_string();
    if raw.is_empty() {
        anyhow::bail!("git rev-parse --git-common-dir returned an empty path");
    }
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        repo_dir.join(path)
    };
    Ok(canonicalize_or_self(&absolute))
}

pub fn repo_scope_key(repo_dir: &Path) -> Result<String> {
    Ok(hash_path_scope(&git_common_dir(repo_dir)?))
}

pub fn worktree_scope_key(repo_dir: &Path) -> Result<String> {
    Ok(hash_path_scope(repo_dir))
}

fn append_untracked_file_fingerprint(state: &mut u64, repo_dir: &Path, relative: &str) {
    stable_hash_str(state, relative);
    let path = repo_dir.join(relative);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            stable_hash_update(state, &meta.len().to_le_bytes());
            match std::fs::read(&path) {
                Ok(bytes) => stable_hash_update(state, &bytes),
                Err(err) => stable_hash_str(state, &format!("read-error:{err}")),
            }
        }
        Ok(meta) => {
            stable_hash_str(state, if meta.is_dir() { "dir" } else { "other" });
        }
        Err(err) => stable_hash_str(state, &format!("missing:{err}")),
    }
}

pub fn current_source_state(repo_dir: &Path) -> Result<SourceState> {
    let short_hash = current_git_hash(repo_dir)?;
    let full_hash = current_git_hash_full(repo_dir)?;
    let status = git_output_bytes(
        repo_dir,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let diff = git_output_bytes(repo_dir, &["diff", "--binary", "HEAD"])?;
    let untracked = git_output_bytes(
        repo_dir,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;

    let dirty = !status.is_empty();
    let changed_paths = status
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .count();

    let mut state = FNV_OFFSET_BASIS_64;
    stable_hash_str(&mut state, &full_hash);
    stable_hash_update(&mut state, &status);
    stable_hash_update(&mut state, &diff);
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let relative = String::from_utf8_lossy(path);
        append_untracked_file_fingerprint(&mut state, repo_dir, &relative);
    }
    let fingerprint = format!("{state:016x}");
    let version_label = if dirty {
        format!("{}-dirty-{}", short_hash, &fingerprint[..12])
    } else {
        short_hash.clone()
    };

    Ok(SourceState {
        repo_scope: repo_scope_key(repo_dir)?,
        worktree_scope: worktree_scope_key(repo_dir)?,
        short_hash,
        full_hash,
        dirty,
        fingerprint,
        version_label,
        changed_paths,
    })
}

pub fn ensure_source_state_matches(repo_dir: &Path, expected: &SourceState) -> Result<SourceState> {
    let current = current_source_state(repo_dir)?;
    if current.fingerprint != expected.fingerprint {
        anyhow::bail!(
            "Source tree drift detected while waiting/building (expected {}, now {}). Refusing to publish or attach this build to the original request.",
            expected.fingerprint,
            current.fingerprint
        );
    }
    Ok(current)
}

fn repo_build_version(repo_dir: &std::path::Path) -> Result<String> {
    Ok(current_source_state(repo_dir)?.version_label)
}

/// Get the current git hash
pub fn current_git_hash(repo_dir: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_dir)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to get git hash");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the full git hash
pub fn current_git_hash_full(repo_dir: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to get git hash");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the git diff for uncommitted changes
pub fn current_git_diff(repo_dir: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(repo_dir)
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if working tree is dirty
pub fn is_working_tree_dirty(repo_dir: &std::path::Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()?;

    Ok(!output.stdout.is_empty())
}

/// Get commit message for a hash
pub fn get_commit_message(repo_dir: &std::path::Path, hash: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%s", hash])
        .current_dir(repo_dir)
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Build info for current state
pub fn current_build_info(repo_dir: &std::path::Path) -> Result<BuildInfo> {
    let source = current_source_state(repo_dir)?;
    let commit_message = get_commit_message(repo_dir, &source.short_hash).ok();

    Ok(BuildInfo {
        hash: source.short_hash,
        full_hash: source.full_hash,
        built_at: Utc::now(),
        commit_message,
        dirty: source.dirty,
        source_fingerprint: Some(source.fingerprint),
        version_label: Some(source.version_label),
    })
}

/// Install a binary at a specific immutable version path.
pub fn install_binary_at_version(source: &std::path::Path, version: &str) -> Result<PathBuf> {
    if !source.exists() {
        anyhow::bail!("Binary not found at {:?}", source);
    }

    let dest_dir = builds_dir()?.join("versions").join(version);
    storage::ensure_dir(&dest_dir)?;

    let dest = dest_dir.join(binary_name());

    // Remove existing file first to avoid ETXTBSY when replacing a running binary.
    if dest.exists() {
        std::fs::remove_file(&dest)?;
    }

    // Prefer hard link (instant, zero I/O) over copy (71MB+ binary).
    // Falls back to copy if hard link fails (e.g. cross-filesystem).
    if std::fs::hard_link(source, &dest).is_err() {
        std::fs::copy(source, &dest)?;
    }
    crate::platform::set_permissions_executable(&dest)?;

    Ok(dest)
}

pub fn smoke_test_binary(binary: &Path) -> Result<()> {
    let output = Command::new(binary)
        .args(["version", "--json"])
        .env("JCODE_NON_INTERACTIVE", "1")
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "Binary smoke test failed for {} with exit code {:?}: {}",
            binary.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        anyhow::anyhow!(
            "Binary smoke test for {} returned invalid JSON: {}",
            binary.display(),
            err
        )
    })?;
    if value.get("version").is_none() {
        anyhow::bail!(
            "Binary smoke test for {} returned JSON without a version field",
            binary.display()
        );
    }
    Ok(())
}

fn update_channel_symlink(channel: &str, version: &str) -> Result<PathBuf> {
    let channel_dir = builds_dir()?.join(channel);
    storage::ensure_dir(&channel_dir)?;

    let link_path = channel_dir.join(binary_name());
    let target = version_binary_path(version)?;
    if !target.exists() {
        anyhow::bail!("Version binary not found at {:?}", target);
    }

    let temp = channel_dir.join(format!(
        ".{}-{}-{}",
        binary_stem(),
        channel,
        std::process::id()
    ));
    crate::platform::atomic_symlink_swap(&target, &link_path, &temp)?;

    Ok(link_path)
}

/// Update stable symlink to point to a version and publish stable-version marker.
pub fn update_stable_symlink(version: &str) -> Result<PathBuf> {
    let stable_link = update_channel_symlink("stable", version)?;
    std::fs::write(stable_version_file()?, version)?;
    Ok(stable_link)
}

/// Update current symlink to point to a version and publish current-version marker.
pub fn update_current_symlink(version: &str) -> Result<PathBuf> {
    let current_link = update_channel_symlink("current", version)?;
    std::fs::write(current_version_file()?, version)?;
    Ok(current_link)
}

pub fn publish_local_current_build_for_source(
    repo_dir: &Path,
    source: &SourceState,
) -> Result<PublishedBuild> {
    let binary = find_dev_binary(repo_dir)
        .ok_or_else(|| anyhow::anyhow!("Binary not found in target/selfdev or target/release"))?;
    if !binary.exists() {
        anyhow::bail!("Binary not found at {:?}", binary);
    }

    smoke_test_binary(&binary)?;
    let previous_current_version = read_current_version()?;
    let versioned_path = install_binary_at_version(&binary, &source.version_label)?;
    smoke_test_binary(&versioned_path)?;
    let current_link = update_current_symlink(&source.version_label)?;
    let launcher_link = update_launcher_symlink_to_current()?;

    Ok(PublishedBuild {
        version: source.version_label.clone(),
        source_fingerprint: source.fingerprint.clone(),
        versioned_path,
        current_link,
        launcher_link,
        previous_current_version,
    })
}

/// Install the local release binary into immutable versions and make it the active `current`
/// build + launcher, while keeping `stable` untouched.
pub fn publish_local_current_build(repo_dir: &std::path::Path) -> Result<PathBuf> {
    let source = current_source_state(repo_dir)?;
    Ok(publish_local_current_build_for_source(repo_dir, &source)?.versioned_path)
}

/// Install release binary into immutable versions, promote it to stable, and also make it the
/// active current/launcher build.
pub fn install_local_release(repo_dir: &std::path::Path) -> Result<PathBuf> {
    let source = release_binary_path(repo_dir);
    if !source.exists() {
        anyhow::bail!("Binary not found at {:?}", source);
    }

    let version = repo_build_version(repo_dir)?;

    let versioned = install_binary_at_version(&source, &version)?;
    update_stable_symlink(&version)?;
    update_current_symlink(&version)?;
    update_launcher_symlink_to_current()?;

    Ok(versioned)
}

/// Save migration context before switching to canary
pub fn save_migration_context(ctx: &MigrationContext) -> Result<()> {
    let path = migration_context_path(&ctx.session_id)?;
    storage::write_json(&path, ctx)
}

/// Load migration context
pub fn load_migration_context(session_id: &str) -> Result<Option<MigrationContext>> {
    let path = migration_context_path(session_id)?;
    if path.exists() {
        Ok(Some(storage::read_json(&path)?))
    } else {
        Ok(None)
    }
}

/// Clear migration context after successful migration
pub fn clear_migration_context(session_id: &str) -> Result<()> {
    let path = migration_context_path(session_id)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Read the current stable version
pub fn read_stable_version() -> Result<Option<String>> {
    let path = stable_version_file()?;
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        let hash = content.trim();
        if hash.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hash.to_string()))
        }
    } else {
        Ok(None)
    }
}

/// Read the current active version.
pub fn read_current_version() -> Result<Option<String>> {
    let path = current_version_file()?;
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        let hash = content.trim();
        if hash.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hash.to_string()))
        }
    } else {
        Ok(None)
    }
}

/// Copy binary to versioned location
pub fn install_version(repo_dir: &std::path::Path, hash: &str) -> Result<PathBuf> {
    let source = release_binary_path(repo_dir);
    install_binary_at_version(&source, hash)
}

/// Update canary symlink to point to a version
pub fn update_canary_symlink(hash: &str) -> Result<()> {
    let _ = update_channel_symlink("canary", hash)?;
    Ok(())
}

/// Get path to build log file
pub fn build_log_path() -> Result<PathBuf> {
    Ok(storage::jcode_dir()?.join("build.log"))
}

/// Get path to build progress file (for TUI to watch)
pub fn build_progress_path() -> Result<PathBuf> {
    Ok(storage::jcode_dir()?.join("build-progress"))
}

/// Write current build progress (for TUI to display)
pub fn write_build_progress(status: &str) -> Result<()> {
    let path = build_progress_path()?;
    std::fs::write(&path, status)?;
    Ok(())
}

/// Read current build progress
pub fn read_build_progress() -> Option<String> {
    build_progress_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Clear build progress
pub fn clear_build_progress() -> Result<()> {
    let path = build_progress_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_jcode_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::storage::lock_test_env();
        let temp_home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp_home.path());
        let result = f();
        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        result
    }

    fn create_git_repo_fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join(".git")).expect("create .git dir");
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"jcode\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp.path())
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp.path())
            .output()
            .expect("git config name");
        std::process::Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(temp.path())
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(temp.path())
            .output()
            .expect("git commit");
        temp
    }

    #[test]
    fn test_build_manifest_default() {
        let manifest = BuildManifest::default();
        assert!(manifest.stable.is_none());
        assert!(manifest.canary.is_none());
        assert!(manifest.history.is_empty());
    }

    #[test]
    fn test_binary_choice_for_canary_session() {
        let manifest = BuildManifest {
            canary: Some("abc123".to_string()),
            canary_session: Some("session_test".to_string()),
            ..Default::default()
        };

        // Canary session should get canary binary
        match manifest.binary_for_session("session_test") {
            BinaryChoice::Canary(hash) => assert_eq!(hash, "abc123"),
            _ => panic!("Expected canary binary"),
        }

        // Other sessions should get stable (or current if no stable)
        match manifest.binary_for_session("other_session") {
            BinaryChoice::Current => {}
            _ => panic!("Expected current binary"),
        }
    }

    #[test]
    fn test_find_repo_in_ancestors_walks_upward() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("jcode-repo");
        let nested = repo.join("a").join("b").join("c");

        std::fs::create_dir_all(repo.join(".git")).expect("create .git");
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"jcode\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");
        std::fs::create_dir_all(&nested).expect("create nested dirs");

        let found = find_repo_in_ancestors(&nested).expect("repo should be found");
        assert_eq!(found, repo);
    }

    #[test]
    fn test_client_update_candidate_prefers_dev_binary_for_selfdev() {
        let _guard = crate::storage::lock_test_env();
        let temp_home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp_home.path());

        let version = "test-current";
        let version_binary =
            install_binary_at_version(std::env::current_exe().as_ref().unwrap(), version)
                .expect("install test version");
        update_current_symlink(version).expect("update current symlink");

        let candidate = client_update_candidate(true).expect("expected selfdev candidate");
        assert_eq!(candidate.1, "current");
        assert_eq!(
            std::fs::canonicalize(candidate.0).expect("canonical candidate"),
            std::fs::canonicalize(version_binary).expect("canonical version binary")
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[test]
    fn launcher_dir_uses_sandbox_bin_when_jcode_home_is_set() {
        with_temp_jcode_home(|| {
            let launcher_dir = launcher_dir().expect("launcher dir");
            let expected = storage::jcode_dir().expect("jcode dir").join("bin");
            assert_eq!(launcher_dir, expected);
        });
    }

    #[test]
    fn update_launcher_symlink_stays_inside_sandbox_home() {
        with_temp_jcode_home(|| {
            let version = "sandbox-current";
            let version_binary =
                install_binary_at_version(std::env::current_exe().as_ref().unwrap(), version)
                    .expect("install test version");
            update_current_symlink(version).expect("update current symlink");

            let launcher = update_launcher_symlink_to_current().expect("update launcher");
            let expected_launcher = storage::jcode_dir()
                .expect("jcode dir")
                .join("bin")
                .join(binary_name());
            assert_eq!(launcher, expected_launcher);
            assert_eq!(
                std::fs::canonicalize(&launcher).expect("canonical launcher"),
                std::fs::canonicalize(version_binary).expect("canonical version binary")
            );
        });
    }

    #[test]
    fn test_canary_status_serialization() {
        assert_eq!(
            serde_json::to_string(&CanaryStatus::Testing).unwrap(),
            "\"testing\""
        );
        assert_eq!(
            serde_json::to_string(&CanaryStatus::Passed).unwrap(),
            "\"passed\""
        );
    }

    #[test]
    fn dirty_source_state_uses_fingerprint_in_version_label() {
        let repo = create_git_repo_fixture();
        std::fs::write(repo.path().join("notes.txt"), "dirty change\n").expect("write dirty file");

        let state = current_source_state(repo.path()).expect("source state");
        assert!(state.dirty);
        assert!(
            state
                .version_label
                .starts_with(&format!("{}-dirty-", state.short_hash))
        );
        assert!(state.version_label.len() > state.short_hash.len() + 7);
    }

    #[test]
    fn pending_activation_can_complete_and_roll_back() {
        with_temp_jcode_home(|| {
            let current_version = "stable-prev";
            install_binary_at_version(std::env::current_exe().as_ref().unwrap(), current_version)
                .expect("install previous version");
            update_current_symlink(current_version).expect("publish previous current");

            let mut manifest = BuildManifest::default();
            manifest
                .set_pending_activation(PendingActivation {
                    session_id: "session-a".to_string(),
                    new_version: "canary-next".to_string(),
                    previous_current_version: Some(current_version.to_string()),
                    source_fingerprint: Some("fingerprint-a".to_string()),
                    requested_at: Utc::now(),
                })
                .expect("set pending activation");

            let completed = complete_pending_activation_for_session("session-a")
                .expect("complete activation")
                .expect("completed version");
            assert_eq!(completed, "canary-next");
            let manifest = BuildManifest::load().expect("load manifest");
            assert!(manifest.pending_activation.is_none());
            assert_eq!(manifest.canary.as_deref(), Some("canary-next"));
            assert_eq!(manifest.canary_status, Some(CanaryStatus::Passed));

            let mut manifest = BuildManifest::load().expect("reload manifest");
            manifest
                .set_pending_activation(PendingActivation {
                    session_id: "session-b".to_string(),
                    new_version: "canary-bad".to_string(),
                    previous_current_version: Some(current_version.to_string()),
                    source_fingerprint: Some("fingerprint-b".to_string()),
                    requested_at: Utc::now(),
                })
                .expect("set second pending activation");

            let rolled_back = rollback_pending_activation_for_session("session-b")
                .expect("rollback activation")
                .expect("rolled back version");
            assert_eq!(rolled_back, "canary-bad");
            let restored = read_current_version()
                .expect("read current version")
                .expect("restored current version");
            assert_eq!(restored, current_version);
        });
    }
}
