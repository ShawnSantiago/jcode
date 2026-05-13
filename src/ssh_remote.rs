use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SshRemoteConfig {
    #[serde(default)]
    pub hosts: Vec<SshRemoteProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SshRemoteProfile {
    pub name: String,
    pub ssh_target: String,
    #[serde(default = "default_workspace")]
    pub workspace: String,
}

fn default_workspace() -> String {
    "~".to_string()
}

pub fn config_path() -> Result<PathBuf> {
    Ok(crate::storage::jcode_dir()?.join("ssh_remotes.json"))
}

pub fn control_dir() -> Result<PathBuf> {
    Ok(crate::storage::jcode_dir()?.join("ssh-control"))
}

pub fn control_socket_path(name: &str) -> Result<PathBuf> {
    Ok(control_dir()?.join(format!("{}.sock", sanitize_profile_name(name))))
}

pub fn load_config() -> Result<SshRemoteConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(SshRemoteConfig::default());
    }
    crate::storage::read_json(&path).with_context(|| format!("failed to read {}", path.display()))
}

pub fn save_config(config: &SshRemoteConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let _ = crate::platform::set_directory_permissions_owner_only(parent);
    }
    let bytes = serde_json::to_vec_pretty(config)?;
    std::fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn upsert_profile(name: &str, ssh_target: &str) -> Result<SshRemoteProfile> {
    let mut config = load_config()?;
    let profile = SshRemoteProfile {
        name: name.to_string(),
        ssh_target: ssh_target.to_string(),
        workspace: default_workspace(),
    };
    if let Some(existing) = config.hosts.iter_mut().find(|p| p.name == name) {
        *existing = profile.clone();
    } else {
        config.hosts.push(profile.clone());
        config.hosts.sort_by(|a, b| a.name.cmp(&b.name));
    }
    save_config(&config)?;
    Ok(profile)
}

pub fn find_profile(name: &str) -> Result<Option<SshRemoteProfile>> {
    Ok(load_config()?.hosts.into_iter().find(|p| p.name == name))
}

pub fn sanitize_profile_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "remote".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn is_control_master_alive(profile: &SshRemoteProfile) -> bool {
    let Ok(socket) = control_socket_path(&profile.name) else {
        return false;
    };
    Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-O")
        .arg("check")
        .arg(&profile.ssh_target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn can_connect_batch_mode(profile: &SshRemoteProfile) -> bool {
    Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg(&profile.ssh_target)
        .arg("true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn disconnect(profile: &SshRemoteProfile) -> Result<bool> {
    let socket = control_socket_path(&profile.name)?;
    let status = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-O")
        .arg("exit")
        .arg(&profile.ssh_target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to run ssh disconnect")?;
    Ok(status.success())
}

pub fn build_control_master_script(profile: &SshRemoteProfile) -> Result<String> {
    std::fs::create_dir_all(control_dir()?)?;
    let socket = control_socket_path(&profile.name)?;
    let target = &profile.ssh_target;
    Ok(format!(
        r#"printf '%s\n' 'Jcode SSH login for {name}'
printf '%s\n' 'Type your SSH password here if prompted. Jcode will not see or store it.'
printf '%s\n' 'After login succeeds, Jcode verifies the SSH control socket before this terminal closes.'
printf '%s\n' 'If verification fails, this terminal will stay open so you can read the error.'
ssh -f -M -S {socket} -N {target}
status=$?
if [ $status -ne 0 ]; then
  printf '%s\n' 'SSH connection failed. Check your username, host, password, school VPN, or two-factor prompt.'
  printf '%s' 'Press Enter to close this terminal... '
  read _
  exit $status
fi

printf '%s\n' 'SSH accepted the login. Verifying background connection...'
for i in 1 2 3 4 5 6 7 8 9 10; do
  if ssh -S {socket} -O check {target} >/dev/null 2>&1; then
    printf '%s\n' 'Connected and verified. Jcode can now use this SSH connection headlessly.'
    sleep 1
    exit 0
  fi
  sleep 1
done

printf '%s\n' 'SSH login appeared to succeed, but Jcode could not verify the background control socket.'
printf '%s\n' 'This can happen if the server disallows SSH multiplexing or the connection closed immediately.'
printf '%s\n' 'The terminal is staying open so you can read this message.'
printf '%s' 'Press Enter to close this terminal... '
read _
exit 1
"#,
        name = shell_single_quote(&profile.name),
        socket = shell_single_quote(&socket.to_string_lossy()),
        target = shell_single_quote(target),
    ))
}

pub fn spawn_control_master_terminal(profile: &SshRemoteProfile) -> Result<bool> {
    let script = build_control_master_script(profile)?;
    let command = crate::terminal_launch::TerminalCommand::new(
        "sh".to_string(),
        vec!["-lc".to_string(), script],
    )
    .title(format!("jcode ssh · {}", profile.name));
    crate::terminal_launch::spawn_command_in_new_terminal(&command, Path::new("."))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_profile_name_keeps_safe_chars() {
        assert_eq!(sanitize_profile_name("school"), "school");
        assert_eq!(
            sanitize_profile_name("alice@login.school.edu"),
            "alice_login.school.edu"
        );
        assert_eq!(sanitize_profile_name("!!!"), "remote");
    }

    #[test]
    fn control_master_script_waits_for_verified_socket_before_closing() {
        let profile = SshRemoteProfile {
            name: "school".to_string(),
            ssh_target: "alice@login.school.edu".to_string(),
            workspace: "~".to_string(),
        };

        let script = build_control_master_script(&profile).unwrap();
        assert!(script.contains("Verifying background connection"));
        assert!(script.contains("ssh -S"));
        assert!(script.contains("-O check"));
        assert!(script.contains("Press Enter to close this terminal"));
        assert!(script.contains("Jcode will not see or store it"));
    }
}
