use super::{
    Server, SessionInterruptQueues, SwarmMember, dispatch_background_task_completion,
    persist_swarm_state_snapshot,
};
use crate::agent::Agent;
use crate::bus::{BackgroundTaskCompleted, BackgroundTaskStatus};
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use crate::protocol::{NotificationType, ServerEvent};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use crate::tool::selfdev::ReloadContext;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::timeout;

struct EnvGuard {
    prev_home: Option<OsString>,
    prev_runtime_dir: Option<OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.prev_home {
            crate::env::set_var("JCODE_HOME", value);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        if let Some(value) = &self.prev_runtime_dir {
            crate::env::set_var("JCODE_RUNTIME_DIR", value);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
    }
}

fn configure_test_env(root: &tempfile::TempDir) -> EnvGuard {
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_runtime_dir = std::env::var_os("JCODE_RUNTIME_DIR");
    let home_dir = root.path().join("home");
    let runtime_dir = root.path().join("runtime");
    std::fs::create_dir_all(&home_dir).expect("create home dir");
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    crate::env::set_var("JCODE_HOME", &home_dir);
    crate::env::set_var("JCODE_RUNTIME_DIR", &runtime_dir);
    EnvGuard {
        prev_home,
        prev_runtime_dir,
    }
}

#[derive(Default, Clone)]
struct StreamingMockProvider {
    responses: Arc<StdMutex<Vec<Vec<StreamEvent>>>>,
}

impl StreamingMockProvider {
    fn queue_response(&self, response: Vec<StreamEvent>) {
        self.responses
            .lock()
            .expect("streaming mock response queue lock")
            .push(response);
    }
}

#[async_trait]
impl Provider for StreamingMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let response = self
            .responses
            .lock()
            .expect("streaming mock response queue lock")
            .remove(0);
        Ok(Box::pin(tokio_stream::iter(response.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn test_agent(provider: Arc<dyn Provider>) -> Arc<Mutex<Agent>> {
    let registry = Registry::new(provider.clone()).await;
    Arc::new(Mutex::new(Agent::new(provider, registry)))
}

fn attached_swarm_member(
    session_id: &str,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
) -> SwarmMember {
    SwarmMember {
        session_id: session_id.to_string(),
        event_tx,
        event_txs: HashMap::new(),
        working_dir: None,
        swarm_id: None,
        swarm_enabled: false,
        status: "ready".to_string(),
        detail: None,
        friendly_name: Some("otter".to_string()),
        report_back_to_session_id: None,
        role: "agent".to_string(),
        joined_at: Instant::now(),
        last_status_change: Instant::now(),
        is_headless: false,
    }
}

fn persisted_headless_member(
    session_id: &str,
    swarm_id: &str,
    status: &str,
    detail: &str,
) -> SwarmMember {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    SwarmMember {
        session_id: session_id.to_string(),
        event_tx,
        event_txs: HashMap::new(),
        working_dir: None,
        swarm_id: Some(swarm_id.to_string()),
        swarm_enabled: true,
        status: status.to_string(),
        detail: Some(detail.to_string()),
        friendly_name: Some(session_id.to_string()),
        report_back_to_session_id: None,
        role: "agent".to_string(),
        joined_at: Instant::now(),
        last_status_change: Instant::now(),
        is_headless: true,
    }
}

#[tokio::test]
async fn background_task_wake_runs_live_session_immediately_when_idle() {
    let provider = Arc::new(StreamingMockProvider::default());
    provider.queue_response(vec![
        StreamEvent::TextDelta("Build result processed.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = test_agent(provider_dyn).await;
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        attached_swarm_member(&session_id, member_event_tx),
    )])));
    let task = BackgroundTaskCompleted {
        task_id: "bgwake".to_string(),
        tool_name: "selfdev-build".to_string(),
        session_id: session_id.clone(),
        status: BackgroundTaskStatus::Completed,
        exit_code: Some(0),
        output_preview: "done\n".to_string(),
        output_file: std::env::temp_dir().join("bgwake.output"),
        duration_secs: 1.4,
        notify: true,
        wake: true,
    };

    dispatch_background_task_completion(&task, &sessions, &soft_interrupt_queues, &swarm_members)
        .await;

    let notification = timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::Notification {
                    notification_type,
                    message,
                    ..
                }) => return (notification_type, message),
                Some(_) => continue,
                None => panic!("member stream closed before notification"),
            }
        }
    })
    .await
    .expect("background task notification should arrive promptly");

    match notification.0 {
        NotificationType::Message { scope, channel } => {
            assert_eq!(scope.as_deref(), Some("background_task"));
            assert!(channel.is_none());
        }
        other => panic!("unexpected notification type: {other:?}"),
    }
    assert!(notification.1.contains("**Background task** `bgwake`"));

    let streamed = timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::TextDelta { text })
                    if text.contains("Build result processed.") =>
                {
                    return text;
                }
                Some(_) => continue,
                None => panic!("member stream closed before wake ran"),
            }
        }
    })
    .await
    .expect("wake delivery should start streaming promptly");
    assert!(streamed.contains("Build result processed."));

    let guard = agent.lock().await;
    assert!(guard.messages().iter().any(|message| {
        message.role == Role::User
            && message
                .content_preview()
                .contains("**Background task** `bgwake`")
    }));
}

#[tokio::test]
async fn background_task_notify_without_wake_does_not_queue_soft_interrupt() {
    let provider: Arc<dyn Provider> = Arc::new(StreamingMockProvider::default());
    let agent = test_agent(provider).await;
    let session_id = agent.lock().await.session_id().to_string();
    let queue = agent.lock().await.soft_interrupt_queue();
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        queue.clone(),
    )])));
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        attached_swarm_member(&session_id, member_event_tx),
    )])));
    let task = BackgroundTaskCompleted {
        task_id: "bgnotify".to_string(),
        tool_name: "bash".to_string(),
        session_id: session_id.clone(),
        status: BackgroundTaskStatus::Completed,
        exit_code: Some(0),
        output_preview: "ok\n".to_string(),
        output_file: std::env::temp_dir().join("bgnotify.output"),
        duration_secs: 0.7,
        notify: true,
        wake: false,
    };

    dispatch_background_task_completion(&task, &sessions, &soft_interrupt_queues, &swarm_members)
        .await;

    let notification = timeout(Duration::from_secs(2), member_event_rx.recv())
        .await
        .expect("background task notification should arrive promptly")
        .expect("member stream should stay open");
    match notification {
        ServerEvent::Notification { message, .. } => {
            assert!(message.contains("**Background task** `bgnotify`"));
        }
        other => panic!("expected notification, got {other:?}"),
    }

    let pending = queue.lock().expect("queue lock");
    assert!(
        pending.is_empty(),
        "notify-only delivery should not wake the session"
    );
}

#[tokio::test]
async fn startup_recovery_resumes_interrupted_headless_sessions_after_reload() -> Result<()> {
    let _storage_guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new()?;
    let _env = configure_test_env(&temp);

    let provider = Arc::new(StreamingMockProvider::default());
    for _ in 0..2 {
        provider.queue_response(vec![
            StreamEvent::TextDelta("continued after reload".to_string()),
            StreamEvent::MessageEnd { stop_reason: None },
        ]);
    }

    let mut initiator = crate::session::Session::create(None, Some("initiator".to_string()));
    initiator.set_canary("self-dev");
    initiator.add_message(
        Role::User,
        vec![crate::message::ContentBlock::ToolResult {
            tool_use_id: "tool_reload".to_string(),
            content: "Reload initiated. Process restarting...".to_string(),
            is_error: Some(false),
        }],
    );
    initiator.save()?;

    ReloadContext {
        task_context: Some("Verify multi-session reload recovery".to_string()),
        version_before: "old-build".to_string(),
        version_after: "new-build".to_string(),
        session_id: initiator.id.clone(),
        timestamp: "2026-04-19T00:00:00Z".to_string(),
    }
    .save()?;

    let mut peer = crate::session::Session::create(None, Some("peer".to_string()));
    peer.add_message(
        Role::User,
        vec![crate::message::ContentBlock::ToolResult {
            tool_use_id: "tool_bash".to_string(),
            content: "[Tool 'bash' interrupted by server reload after 0.2s]".to_string(),
            is_error: Some(true),
        }],
    );
    peer.save()?;

    let swarm_id = "swarm-reload-recovery";
    persist_swarm_state_snapshot(
        swarm_id,
        None,
        None,
        &[
            persisted_headless_member(&initiator.id, swarm_id, "running", "selfdev reload"),
            persisted_headless_member(&peer.id, swarm_id, "running", "bash tool"),
        ],
    );

    let server = Server::new(provider.clone());
    {
        let members = server.swarm_state.members.read().await;
        assert_eq!(
            members
                .get(&initiator.id)
                .map(|member| member.status.as_str()),
            Some("crashed"),
            "persisted running headless sessions should load as crashed before recovery"
        );
        assert_eq!(
            members.get(&peer.id).map(|member| member.status.as_str()),
            Some("crashed")
        );
    }

    server.recover_headless_sessions_on_startup().await;

    timeout(Duration::from_secs(5), async {
        loop {
            let sessions = server.sessions.read().await;
            let Some(initiator_agent) = sessions.get(&initiator.id).cloned() else {
                drop(sessions);
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            let Some(peer_agent) = sessions.get(&peer.id).cloned() else {
                drop(sessions);
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            drop(sessions);

            let initiator_done = {
                let guard = initiator_agent.lock().await;
                guard.messages().iter().any(|message| {
                    message.role == Role::Assistant
                        && message.content_preview().contains("continued after reload")
                })
            };
            let peer_done = {
                let guard = peer_agent.lock().await;
                guard.messages().iter().any(|message| {
                    message.role == Role::Assistant
                        && message.content_preview().contains("continued after reload")
                })
            };
            let statuses_ready = {
                let members = server.swarm_state.members.read().await;
                members
                    .get(&initiator.id)
                    .map(|member| member.status.as_str())
                    == Some("ready")
                    && members.get(&peer.id).map(|member| member.status.as_str()) == Some("ready")
            };

            if initiator_done && peer_done && statuses_ready {
                break;
            }

            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("headless reload recovery should resume both sessions");

    assert!(
        ReloadContext::peek_for_session(&initiator.id)?.is_none(),
        "initiator reload context should be consumed by headless recovery"
    );

    Ok(())
}
