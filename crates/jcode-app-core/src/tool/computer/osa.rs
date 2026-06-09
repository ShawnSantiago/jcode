//! Centralized `osascript` / JXA execution for the `macos_computer_use` tool.
//!
//! Many macOS capabilities (Accessibility actions, window/app management, system
//! state) are reachable through AppleScript / JavaScript-for-Automation without
//! extra native bindings. This module funnels all of that through one place so
//! escaping, error mapping (especially the TCC permission errors), and timeouts
//! are handled consistently.
//!
//! Every external command runs under a wall-clock timeout: a hung target app
//! must never freeze the agent. AppleScript also gets an internal
//! `with timeout` guard so System Events stops waiting on an unresponsive app.

use anyhow::{Result, bail};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default wall-clock limit for a scripting call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Run an AppleScript and return stdout (trimmed). Maps the common macOS
/// permission / automation errors to actionable messages.
pub fn run_applescript(script: &str) -> Result<String> {
    run(&["-e", script], "AppleScript", DEFAULT_TIMEOUT)
}

/// Run AppleScript with an explicit timeout.
pub fn run_applescript_timeout(script: &str, timeout: Duration) -> Result<String> {
    run(&["-e", script], "AppleScript", timeout)
}

/// Run a JavaScript-for-Automation (JXA) script.
pub fn run_jxa(script: &str) -> Result<String> {
    run(&["-l", "JavaScript", "-e", script], "JXA", DEFAULT_TIMEOUT)
}

fn run(args: &[&str], lang: &str, timeout: Duration) -> Result<String> {
    let (status, stdout, stderr) = run_command_timed("/usr/bin/osascript", args, timeout)?;

    if status {
        return Ok(stdout.trim_end().to_string());
    }

    let trimmed = stderr.trim();
    let lower = trimmed.to_lowercase();

    if lower.contains("assistive")
        || lower.contains("not allowed")
        || lower.contains("-1719")
        || lower.contains("1002")
    {
        bail!(
            "Accessibility permission required. Run the `setup` action, or grant it in \
             System Settings > Privacy & Security > Accessibility for your terminal/jcode. \
             ({trimmed})"
        );
    }
    if lower.contains("-1743") || lower.contains("not authorized to send apple events") {
        bail!(
            "Automation permission required for the target app. Approve the prompt, or grant it \
             in System Settings > Privacy & Security > Automation. ({trimmed})"
        );
    }
    if trimmed.is_empty() {
        bail!("{lang} failed (no error output)");
    }
    bail!("{lang} failed: {trimmed}");
}

/// Run a command with a wall-clock timeout. Returns (success, stdout, stderr).
/// On timeout the child is killed and an error is returned.
pub fn run_command_timed(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {program}: {e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Ok((status.success(), out, err));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "command timed out after {}s (a target app may be unresponsive): {program}",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => bail!("error waiting on {program}: {e}"),
        }
    }
}

/// Quote a string as an AppleScript string literal (wraps in quotes, escapes
/// backslash and double-quote). Use for interpolating untrusted text into
/// generated AppleScript.
pub fn as_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(as_quote("hi"), "\"hi\"");
        assert_eq!(as_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(as_quote("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn timed_command_succeeds_fast() {
        let (ok, out, _) = run_command_timed("/bin/echo", &["hi"], Duration::from_secs(5)).unwrap();
        assert!(ok);
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn timed_command_times_out() {
        let err = run_command_timed("/bin/sleep", &["5"], Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "got: {err}");
    }
}
