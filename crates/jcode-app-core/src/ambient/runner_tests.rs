use super::{
    AmbientRunnerHandle, compute_post_cycle_sleep_secs, compute_sleep_secs_until_next_check,
    has_ready_direct_items, persist_scheduled_wake,
};
use crate::ambient::ScheduledQueue;
use crate::ambient::{
    AmbientManager, AmbientState, AmbientStatus, Priority, ScheduleRequest, ScheduleTarget,
    ScheduledItem,
};
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::session::Session;
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

struct TestProvider;

fn scheduled_item(
    id: &str,
    target: ScheduleTarget,
    scheduled_for: chrono::DateTime<chrono::Utc>,
) -> ScheduledItem {
    ScheduledItem {
        id: id.to_string(),
        scheduled_for,
        context: "Follow up later".to_string(),
        priority: Priority::Normal,
        target,
        created_by_session: "session_test".to_string(),
        created_at: chrono::Utc::now(),
        working_dir: None,
        task_description: Some("Follow up later".to_string()),
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
        schedule_key: None,
        schedule_kind: None,
        schedule_payload: None,
    }
}

#[derive(Clone, Default)]
struct StreamingTestProvider {
    responses: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
}

impl StreamingTestProvider {
    fn queue_response(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }
}

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "TestProvider should not be used for streaming completions in ambient runner tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

#[async_trait]
impl Provider for StreamingTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let stream = stream! {
            for event in events {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn runner_stays_alive_to_service_schedules_when_ambient_disabled() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let task = tokio::spawn(runner.clone().run_loop(provider));

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        runner.is_running().await,
        "runner should remain active for scheduled tasks even with ambient disabled"
    );

    task.abort();
    let _ = task.await;
}

#[test]
fn ready_direct_detection_ignores_ambient_and_future_items() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut queue = ScheduledQueue::load(temp.path().join("queue.json"));
    let now = chrono::Utc::now();

    queue.push(scheduled_item(
        "ambient_ready",
        ScheduleTarget::Ambient,
        now,
    ));
    queue.push(scheduled_item(
        "spawn_future",
        ScheduleTarget::Spawn {
            parent_session_id: "session_parent".to_string(),
        },
        now + chrono::Duration::minutes(5),
    ));

    assert!(
        !has_ready_direct_items(&queue, now),
        "ambient-ready and future direct items should not bypass user-active pause"
    );

    queue.push(scheduled_item(
        "spawn_ready",
        ScheduleTarget::Spawn {
            parent_session_id: "session_parent".to_string(),
        },
        now,
    ));

    assert!(
        has_ready_direct_items(&queue, now),
        "ready spawn/resume items must bypass user-active pause"
    );
}

#[test]
fn direct_due_clamps_ambient_allowed_sleep() {
    let now = chrono::Utc::now();
    let direct_due = now + chrono::Duration::seconds(300);

    let sleep_secs = compute_sleep_secs_until_next_check(now, true, 7200, Some(direct_due));

    assert_eq!(sleep_secs, 300);
}

#[test]
fn overdue_direct_item_wakes_in_one_second() {
    let now = chrono::Utc::now();
    let direct_due = now - chrono::Duration::seconds(30);

    let sleep_secs = compute_sleep_secs_until_next_check(now, true, 7200, Some(direct_due));

    assert_eq!(sleep_secs, 1);
}

#[test]
fn no_direct_item_preserves_adaptive_floor() {
    let now = chrono::Utc::now();

    assert_eq!(
        compute_sleep_secs_until_next_check(now, true, 7200, None),
        7200
    );
    assert_eq!(compute_sleep_secs_until_next_check(now, true, 5, None), 30);
}

#[test]
fn ambient_disabled_sleep_preserves_poll_cap_but_respects_direct_due() {
    let now = chrono::Utc::now();

    assert_eq!(
        compute_sleep_secs_until_next_check(
            now,
            false,
            7200,
            Some(now + chrono::Duration::seconds(300))
        ),
        30
    );
    assert_eq!(
        compute_sleep_secs_until_next_check(
            now,
            false,
            7200,
            Some(now + chrono::Duration::seconds(10))
        ),
        10
    );
    assert_eq!(
        compute_sleep_secs_until_next_check(now, false, 7200, None),
        30
    );
}

#[test]
fn post_cycle_sleep_uses_fresh_direct_queue_and_persists_clamped_wake() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let now = chrono::Utc::now();
    let direct_due = now + chrono::Duration::seconds(240);

    let mut manager = AmbientManager::new().expect("ambient manager");
    manager
        .schedule(ScheduleRequest {
            wake_in_minutes: None,
            wake_at: Some(direct_due),
            context: "Follow up later".to_string(),
            priority: Priority::Normal,
            target: ScheduleTarget::Spawn {
                parent_session_id: "session_parent".to_string(),
            },
            created_by_session: "session_test".to_string(),
            working_dir: None,
            task_description: Some("Follow up later".to_string()),
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
            schedule_key: None,
            schedule_kind: None,
            schedule_payload: None,
        })
        .expect("schedule direct item");

    let sleep_secs = compute_post_cycle_sleep_secs(now, true, 7200);
    assert_eq!(sleep_secs, 240);

    let mut state = AmbientState {
        status: AmbientStatus::Idle,
        ..Default::default()
    };
    let next_wake = now + chrono::Duration::seconds(sleep_secs as i64);
    persist_scheduled_wake(next_wake, &mut state).expect("persist scheduled wake");

    let reloaded = AmbientState::load().expect("load ambient state");
    let AmbientStatus::Scheduled { next_wake } = reloaded.status else {
        panic!("expected scheduled wake state");
    };
    assert!(
        next_wake <= direct_due + chrono::Duration::seconds(1),
        "next_wake {next_wake} should be at or just after direct due {direct_due}"
    );
    assert!(
        next_wake < now + chrono::Duration::seconds(7200),
        "clamped next wake should be far earlier than adaptive interval"
    );
    assert!(
        next_wake > now,
        "post-cycle sleep should not persist an immediate or past wake for future direct items"
    );
}

#[tokio::test]
async fn spawn_target_creates_one_child_session_and_runs_task() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider = StreamingTestProvider::default();
    provider.queue_response(vec![
        StreamEvent::TextDelta("Spawned session handled task.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider: Arc<dyn Provider> = Arc::new(provider);

    let mut parent = Session::create_with_id(
        "session_parent_spawn_test".to_string(),
        None,
        Some("Parent".to_string()),
    );
    parent.working_dir = Some(temp.path().display().to_string());
    parent.save().expect("save parent session");

    let item = ScheduledItem {
        id: "sched_spawn_test".to_string(),
        scheduled_for: chrono::Utc::now(),
        context: "Follow up later".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Spawn {
            parent_session_id: parent.id.clone(),
        },
        created_by_session: parent.id.clone(),
        created_at: chrono::Utc::now(),
        working_dir: parent.working_dir.clone(),
        task_description: Some("Follow up later".to_string()),
        relevant_files: vec!["src/lib.rs".to_string()],
        git_branch: None,
        additional_context: Some("Background: spawned schedule test".to_string()),
        schedule_key: None,
        schedule_kind: None,
        schedule_payload: None,
    };

    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let child_session_id = runner
        .spawn_session_for_scheduled_item(&provider, &item, &parent.id)
        .await
        .expect("spawned scheduled task should succeed");

    assert_ne!(child_session_id, parent.id);

    let child = Session::load(&child_session_id).expect("load spawned child session");
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.working_dir, parent.working_dir);
    assert!(child.messages.iter().any(|message| {
        message.role == Role::User
            && message.content_preview().contains("[Scheduled task]")
            && message.content_preview().contains("Follow up later")
    }));
    assert!(child.messages.iter().any(|message| {
        message.role == Role::Assistant
            && message
                .content_preview()
                .contains("Spawned session handled task.")
    }));
}
