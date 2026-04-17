use super::{SessionInterruptQueues, SwarmMember, dispatch_background_task_completion};
use crate::agent::Agent;
use crate::bus::{BackgroundTaskCompleted, BackgroundTaskStatus};
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use crate::protocol::{NotificationType, ServerEvent};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::timeout;

#[derive(Default)]
struct StreamingMockProvider {
    responses: StdMutex<Vec<Vec<StreamEvent>>>,
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
        Arc::new(Self::default())
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
