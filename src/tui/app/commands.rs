use super::{App, DisplayMessage};
use crate::message::Role;
use crate::session::Session;
use std::io::Write;

pub(super) fn reset_current_session(app: &mut App) {
    app.clear_provider_messages();
    app.clear_display_messages();
    app.queued_messages.clear();
    app.pasted_contents.clear();
    app.pending_images.clear();
    app.active_skill = None;
    let mut session = Session::create(None, None);
    session.model = Some(app.provider.model());
    app.session = session;
    app.provider_session_id = None;
}

pub(super) fn handle_help_command(app: &mut App, trimmed: &str) -> bool {
    if let Some(topic) = trimmed
        .strip_prefix("/help ")
        .or_else(|| trimmed.strip_prefix("/? "))
    {
        if let Some(help) = app.command_help(topic) {
            app.push_display_message(DisplayMessage::system(help));
        } else {
            app.push_display_message(DisplayMessage::error(format!(
                "Unknown command '{}'. Use `/help` to list commands.",
                topic.trim()
            )));
        }
        return true;
    }

    if trimmed == "/help" || trimmed == "/?" || trimmed == "/commands" {
        app.help_scroll = Some(0);
        return true;
    }

    false
}

pub(super) fn handle_session_command(app: &mut App, trimmed: &str) -> bool {
    if trimmed == "/clear" {
        reset_current_session(app);
        return true;
    }

    if trimmed == "/save" || trimmed.starts_with("/save ") {
        let label = trimmed.strip_prefix("/save").unwrap().trim();
        let label = if label.is_empty() {
            None
        } else {
            Some(label.to_string())
        };
        app.session.mark_saved(label.clone());
        if let Err(e) = app.session.save() {
            app.push_display_message(DisplayMessage::error(format!(
                "Failed to save session: {}",
                e
            )));
            return true;
        }
        let name = app.session.display_name().to_string();
        let msg = if let Some(ref lbl) = app.session.save_label {
            format!(
                "📌 Session **{}** saved as \"**{}**\". It will appear at the top of `/resume`.",
                name, lbl,
            )
        } else {
            format!(
                "📌 Session **{}** saved. It will appear at the top of `/resume`.",
                name,
            )
        };
        app.push_display_message(DisplayMessage::system(msg));
        app.set_status_notice("Session saved");
        return true;
    }

    if trimmed == "/unsave" {
        app.session.unmark_saved();
        if let Err(e) = app.session.save() {
            app.push_display_message(DisplayMessage::error(format!(
                "Failed to save session: {}",
                e
            )));
            return true;
        }
        let name = app.session.display_name().to_string();
        app.push_display_message(DisplayMessage::system(format!(
            "Removed bookmark from session **{}**.",
            name,
        )));
        app.set_status_notice("Bookmark removed");
        return true;
    }

    if trimmed == "/memory status" {
        let default_enabled = crate::config::config().features.memory;
        app.push_display_message(DisplayMessage::system(format!(
            "Memory feature: **{}** (config default: {})",
            if app.memory_enabled {
                "enabled"
            } else {
                "disabled"
            },
            if default_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )));
        return true;
    }

    if trimmed == "/memory" {
        let new_state = !app.memory_enabled;
        app.set_memory_feature_enabled(new_state);
        let label = if new_state { "ON" } else { "OFF" };
        app.set_status_notice(&format!("Memory: {}", label));
        app.push_display_message(DisplayMessage::system(format!(
            "Memory feature {} for this session.",
            if new_state { "enabled" } else { "disabled" }
        )));
        return true;
    }

    if trimmed == "/memory on" {
        app.set_memory_feature_enabled(true);
        app.set_status_notice("Memory: ON");
        app.push_display_message(DisplayMessage::system(
            "Memory feature enabled for this session.".to_string(),
        ));
        return true;
    }

    if trimmed == "/memory off" {
        app.set_memory_feature_enabled(false);
        app.set_status_notice("Memory: OFF");
        app.push_display_message(DisplayMessage::system(
            "Memory feature disabled for this session.".to_string(),
        ));
        return true;
    }

    if trimmed.starts_with("/memory ") {
        app.push_display_message(DisplayMessage::error(
            "Usage: `/memory [on|off|status]`".to_string(),
        ));
        return true;
    }

    if trimmed == "/swarm" || trimmed == "/swarm status" {
        let default_enabled = crate::config::config().features.swarm;
        app.push_display_message(DisplayMessage::system(format!(
            "Swarm feature: **{}** (config default: {})",
            if app.swarm_enabled {
                "enabled"
            } else {
                "disabled"
            },
            if default_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )));
        return true;
    }

    if trimmed == "/swarm on" {
        app.set_swarm_feature_enabled(true);
        app.set_status_notice("Swarm: ON");
        app.push_display_message(DisplayMessage::system(
            "Swarm feature enabled for this session.".to_string(),
        ));
        return true;
    }

    if trimmed == "/swarm off" {
        app.set_swarm_feature_enabled(false);
        app.set_status_notice("Swarm: OFF");
        app.push_display_message(DisplayMessage::system(
            "Swarm feature disabled for this session.".to_string(),
        ));
        return true;
    }

    if trimmed.starts_with("/swarm ") {
        app.push_display_message(DisplayMessage::error(
            "Usage: `/swarm [on|off|status]`".to_string(),
        ));
        return true;
    }

    if trimmed == "/rewind" {
        if app.session.messages.is_empty() {
            app.push_display_message(DisplayMessage::system(
                "No messages in conversation.".to_string(),
            ));
            return true;
        }

        let mut history = String::from("**Conversation history:**\n\n");
        for (i, msg) in app.session.messages.iter().enumerate() {
            let role_str = match msg.role {
                Role::User => "👤 User",
                Role::Assistant => "🤖 Assistant",
            };
            let content = msg.content_preview();
            let preview = crate::util::truncate_str(&content, 80);
            history.push_str(&format!("  `{}` {} - {}\n", i + 1, role_str, preview));
        }
        history.push_str("\nUse `/rewind N` to rewind to message N (removes all messages after).");

        app.push_display_message(DisplayMessage::system(history));
        return true;
    }

    if let Some(num_str) = trimmed.strip_prefix("/rewind ") {
        let num_str = num_str.trim();
        match num_str.parse::<usize>() {
            Ok(n) if n > 0 && n <= app.session.messages.len() => {
                let removed = app.session.messages.len() - n;
                app.session.messages.truncate(n);
                app.replace_provider_messages(app.session.messages_for_provider());
                app.session.updated_at = chrono::Utc::now();

                app.clear_display_messages();
                for rendered in crate::session::render_messages(&app.session) {
                    app.push_display_message(DisplayMessage {
                        role: rendered.role,
                        content: rendered.content,
                        tool_calls: rendered.tool_calls,
                        duration_secs: None,
                        title: None,
                        tool_data: rendered.tool_data,
                    });
                }

                app.provider_session_id = None;
                app.session.provider_session_id = None;
                let _ = app.session.save();

                app.push_display_message(DisplayMessage::system(format!(
                    "✓ Rewound to message {}. Removed {} message{}.",
                    n,
                    removed,
                    if removed == 1 { "" } else { "s" }
                )));
            }
            Ok(n) => {
                app.push_display_message(DisplayMessage::error(format!(
                    "Invalid message number: {}. Valid range: 1-{}",
                    n,
                    app.session.messages.len()
                )));
            }
            Err(_) => {
                app.push_display_message(DisplayMessage::error(format!(
                    "Usage: `/rewind N` where N is a message number (1-{})",
                    app.session.messages.len()
                )));
            }
        }
        return true;
    }

    false
}

pub(super) fn handle_utility_command(app: &mut App, trimmed: &str) -> bool {
    if trimmed == "/config" {
        use crate::config::config;
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: config().display_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    if trimmed == "/config init" || trimmed == "/config create" {
        use crate::config::Config;
        match Config::create_default_config_file() {
            Ok(path) => {
                app.push_display_message(DisplayMessage {
                    role: "system".to_string(),
                    content: format!(
                        "Created default config file at:\n`{}`\n\nEdit this file to customize your keybindings and settings.",
                        path.display()
                    ),
                    tool_calls: vec![],
                    duration_secs: None,
                    title: None,
                    tool_data: None,
                });
            }
            Err(e) => {
                app.push_display_message(DisplayMessage {
                    role: "system".to_string(),
                    content: format!("Failed to create config file: {}", e),
                    tool_calls: vec![],
                    duration_secs: None,
                    title: None,
                    tool_data: None,
                });
            }
        }
        return true;
    }

    if trimmed == "/config edit" {
        use crate::config::Config;
        if let Some(path) = Config::path() {
            if !path.exists() {
                if let Err(e) = Config::create_default_config_file() {
                    app.push_display_message(DisplayMessage {
                        role: "system".to_string(),
                        content: format!("Failed to create config file: {}", e),
                        tool_calls: vec![],
                        duration_secs: None,
                        title: None,
                        tool_data: None,
                    });
                    return true;
                }
            }

            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
            app.push_display_message(DisplayMessage {
                role: "system".to_string(),
                content: format!(
                    "Opening config in editor...\n`{} {}`\n\n*Restart jcode after editing for changes to take effect.*",
                    editor,
                    path.display()
                ),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            });

            let _ = std::process::Command::new(&editor).arg(&path).spawn();
        }
        return true;
    }

    if trimmed == "/debug-visual" || trimmed == "/debug-visual on" {
        use super::super::visual_debug;
        visual_debug::enable();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "Visual debugging enabled. Frames are being captured.\n\
                     Use `/debug-visual dump` to write captured frames to file.\n\
                     Use `/debug-visual off` to disable."
                .to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        app.set_status_notice("Visual debug: ON");
        return true;
    }

    if trimmed == "/debug-visual off" {
        use super::super::visual_debug;
        visual_debug::disable();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "Visual debugging disabled.".to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        app.set_status_notice("Visual debug: OFF");
        return true;
    }

    if trimmed == "/debug-visual dump" {
        use super::super::visual_debug;
        match visual_debug::dump_to_file() {
            Ok(path) => {
                app.push_display_message(DisplayMessage {
                    role: "system".to_string(),
                    content: format!(
                        "Visual debug dump written to:\n`{}`\n\n\
                         This file contains frame captures with:\n\
                         - Layout computations\n\
                         - State snapshots\n\
                         - Rendered text content\n\
                         - Any detected anomalies",
                        path.display()
                    ),
                    tool_calls: vec![],
                    duration_secs: None,
                    title: None,
                    tool_data: None,
                });
            }
            Err(e) => {
                app.push_display_message(DisplayMessage {
                    role: "error".to_string(),
                    content: format!("Failed to write visual debug dump: {}", e),
                    tool_calls: vec![],
                    duration_secs: None,
                    title: None,
                    tool_data: None,
                });
            }
        }
        return true;
    }

    if trimmed == "/screenshot-mode" || trimmed == "/screenshot-mode on" {
        use super::super::screenshot;
        screenshot::enable();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "Screenshot mode enabled.\n\n\
                     Run the watcher in another terminal:\n\
                     ```bash\n\
                     ./scripts/screenshot_watcher.sh\n\
                     ```\n\n\
                     Use `/screenshot <state>` to trigger a capture.\n\
                     Use `/screenshot-mode off` to disable."
                .to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    if trimmed == "/screenshot-mode off" {
        use super::super::screenshot;
        screenshot::disable();
        screenshot::clear_all_signals();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "Screenshot mode disabled.".to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    if trimmed.starts_with("/screenshot ") {
        use super::super::screenshot;
        let state_name = trimmed.strip_prefix("/screenshot ").unwrap_or("").trim();
        if !state_name.is_empty() {
            screenshot::signal_ready(
                state_name,
                serde_json::json!({
                    "manual_trigger": true,
                }),
            );
            app.push_display_message(DisplayMessage {
                role: "system".to_string(),
                content: format!("Screenshot signal sent: {}", state_name),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            });
        }
        return true;
    }

    if trimmed == "/record" || trimmed == "/record start" {
        use super::super::test_harness;
        test_harness::start_recording();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "🎬 Recording started.\n\n\
                     All your keystrokes are now being recorded.\n\
                     Use `/record stop` to stop and save.\n\
                     Use `/record cancel` to discard."
                .to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    if trimmed == "/record stop" {
        use super::super::test_harness;
        test_harness::stop_recording();
        let json = test_harness::get_recorded_events_json();
        let event_count = json.matches("\"type\"").count();

        let recording_dir = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("jcode")
            .join("recordings");
        let _ = std::fs::create_dir_all(&recording_dir);

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!("recording_{}.json", timestamp);
        let filepath = recording_dir.join(&filename);

        if let Ok(mut file) = std::fs::File::create(&filepath) {
            let _ = file.write_all(json.as_bytes());
        }

        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: format!(
                "🎬 Recording stopped.\n\n\
                 **Events recorded:** {}\n\
                 **Saved to:** `{}`\n\n\
                 To replay as video, run:\n\
                 ```bash\n\
                 ./scripts/replay_recording.sh {}\n\
                 ```",
                event_count,
                filepath.display(),
                filepath.display()
            ),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    if trimmed == "/record cancel" {
        use super::super::test_harness;
        test_harness::stop_recording();
        app.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: "🎬 Recording cancelled.".to_string(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
        return true;
    }

    false
}
