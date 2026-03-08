use super::client_lifecycle::process_message_streaming_mpsc;
use super::{
    broadcast_swarm_plan, broadcast_swarm_status, create_headless_session, record_swarm_event,
    record_swarm_event_for_session, remove_plan_participant, truncate_detail, update_member_status,
    SwarmEvent, SwarmEventType, SwarmMember, VersionedPlan,
};
use crate::agent::Agent;
use crate::protocol::ServerEvent;
use crate::provider::Provider;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_comm_spawn(
    id: u64,
    req_session_id: String,
    working_dir: Option<String>,
    initial_message: Option<String>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    sessions: &Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    global_session_id: &Arc<RwLock<String>>,
    provider_template: &Arc<dyn Provider>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    event_history: &Arc<RwLock<Vec<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
) {
    let swarm_id = match require_coordinator_swarm(
        id,
        &req_session_id,
        "Only the coordinator can spawn new agents.",
        client_event_tx,
        swarm_members,
        swarm_coordinators,
    )
    .await
    {
        Some(swarm_id) => swarm_id,
        None => return,
    };

    let cmd = if let Some(ref dir) = working_dir {
        format!("create_session:{dir}")
    } else {
        "create_session".to_string()
    };
    let coordinator_model = {
        let agent_sessions = sessions.read().await;
        agent_sessions.get(&req_session_id).and_then(|agent| {
            agent
                .try_lock()
                .ok()
                .map(|agent_guard| agent_guard.provider_model())
        })
    };

    match create_headless_session(
        sessions,
        global_session_id,
        provider_template,
        &cmd,
        swarm_members,
        swarms_by_id,
        swarm_coordinators,
        swarm_plans,
        coordinator_model,
        Some(Arc::clone(mcp_pool)),
    )
    .await
    {
        Ok(result_json) => {
            let new_session_id = serde_json::from_str::<serde_json::Value>(&result_json)
                .ok()
                .and_then(|value| {
                    value
                        .get("session_id")
                        .and_then(|session_id| session_id.as_str())
                        .map(|session_id| session_id.to_string())
                })
                .unwrap_or_default();

            {
                let mut plans = swarm_plans.write().await;
                if let Some(plan) = plans.get_mut(&swarm_id) {
                    if !plan.items.is_empty() || !plan.participants.is_empty() {
                        plan.participants.insert(req_session_id.clone());
                        plan.participants.insert(new_session_id.clone());
                    }
                }
            }

            broadcast_swarm_plan(
                &swarm_id,
                Some("participant_spawned".to_string()),
                swarm_plans,
                swarm_members,
                swarms_by_id,
            )
            .await;
            record_swarm_event_for_session(
                &new_session_id,
                SwarmEventType::MemberChange {
                    action: "joined".to_string(),
                },
                swarm_members,
                event_history,
                event_counter,
                swarm_event_tx,
            )
            .await;

            if let Some(initial_msg) = initial_message {
                let agent_arc = {
                    let agent_sessions = sessions.read().await;
                    agent_sessions.get(&new_session_id).cloned()
                };
                if let Some(agent_arc) = agent_arc {
                    let sid_clone = new_session_id.clone();
                    let swarm_members2 = Arc::clone(swarm_members);
                    let swarms_by_id2 = Arc::clone(swarms_by_id);
                    let event_history2 = Arc::clone(event_history);
                    let event_counter2 = Arc::clone(event_counter);
                    let swarm_event_tx2 = swarm_event_tx.clone();
                    tokio::spawn(async move {
                        update_member_status(
                            &sid_clone,
                            "running",
                            Some(truncate_detail(&initial_msg, 120)),
                            &swarm_members2,
                            &swarms_by_id2,
                            Some(&event_history2),
                            Some(&event_counter2),
                            Some(&swarm_event_tx2),
                        )
                        .await;
                        let (drain_tx, mut drain_rx) =
                            tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
                        tokio::spawn(async move { while drain_rx.recv().await.is_some() {} });
                        let result = process_message_streaming_mpsc(
                            Arc::clone(&agent_arc),
                            &initial_msg,
                            vec![],
                            drain_tx,
                        )
                        .await;
                        let (new_status, new_detail) = match result {
                            Ok(()) => ("ready", None),
                            Err(ref error) => {
                                ("failed", Some(truncate_detail(&error.to_string(), 120)))
                            }
                        };
                        update_member_status(
                            &sid_clone,
                            new_status,
                            new_detail,
                            &swarm_members2,
                            &swarms_by_id2,
                            Some(&event_history2),
                            Some(&event_counter2),
                            Some(&swarm_event_tx2),
                        )
                        .await;
                    });
                }
            }

            let _ = client_event_tx.send(ServerEvent::CommSpawnResponse {
                id,
                session_id: req_session_id,
                new_session_id,
            });
        }
        Err(error) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to spawn agent: {error}"),
                retry_after_secs: None,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_comm_stop(
    id: u64,
    req_session_id: String,
    target_session: String,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    sessions: &Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    event_history: &Arc<RwLock<Vec<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) {
    if require_coordinator_swarm(
        id,
        &req_session_id,
        "Only the coordinator can stop agents.",
        client_event_tx,
        swarm_members,
        swarm_coordinators,
    )
    .await
    .is_none()
    {
        return;
    }

    let mut sessions_guard = sessions.write().await;
    let removed_agent = sessions_guard.remove(&target_session);
    drop(sessions_guard);
    if let Some(agent_arc) = removed_agent {
        if let Ok(agent) = agent_arc.try_lock() {
            let memory_enabled = agent.memory_enabled();
            let transcript = if memory_enabled {
                Some(agent.build_transcript_for_extraction())
            } else {
                None
            };
            let sid = target_session.clone();
            drop(agent);
            if let Some(transcript) = transcript {
                crate::memory_agent::trigger_final_extraction(transcript, sid);
            }
        }

        let (removed_swarm_id, removed_name) = {
            let mut members = swarm_members.write().await;
            if let Some(member) = members.remove(&target_session) {
                (member.swarm_id, member.friendly_name)
            } else {
                (None, None)
            }
        };
        if let Some(ref swarm_id) = removed_swarm_id {
            record_swarm_event(
                event_history,
                event_counter,
                swarm_event_tx,
                target_session.clone(),
                removed_name.clone(),
                Some(swarm_id.clone()),
                SwarmEventType::MemberChange {
                    action: "left".to_string(),
                },
            )
            .await;
            remove_plan_participant(swarm_id, &target_session, swarm_plans).await;
            {
                let mut swarms = swarms_by_id.write().await;
                if let Some(swarm) = swarms.get_mut(swarm_id) {
                    swarm.remove(&target_session);
                    if swarm.is_empty() {
                        swarms.remove(swarm_id);
                    }
                }
            }
            let was_coordinator = {
                let coordinators = swarm_coordinators.read().await;
                coordinators
                    .get(swarm_id)
                    .map(|coordinator| coordinator == &target_session)
                    .unwrap_or(false)
            };
            if was_coordinator {
                let new_coordinator = {
                    let swarms = swarms_by_id.read().await;
                    swarms
                        .get(swarm_id)
                        .and_then(|swarm| swarm.iter().min().cloned())
                };
                let mut coordinators = swarm_coordinators.write().await;
                coordinators.remove(swarm_id);
                if let Some(ref new_id) = new_coordinator {
                    coordinators.insert(swarm_id.clone(), new_id.clone());
                    let mut members = swarm_members.write().await;
                    if let Some(member) = members.get_mut(new_id) {
                        member.role = "coordinator".to_string();
                    }
                    let mut plans = swarm_plans.write().await;
                    if let Some(plan) = plans.get_mut(swarm_id) {
                        plan.participants.insert(new_id.clone());
                    }
                }
            }
            broadcast_swarm_status(swarm_id, swarm_members, swarms_by_id).await;
        }
        let _ = client_event_tx.send(ServerEvent::Done { id });
    } else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Unknown session '{target_session}'"),
            retry_after_secs: None,
        });
    }
}

async fn require_coordinator_swarm(
    id: u64,
    req_session_id: &str,
    permission_error: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
) -> Option<String> {
    let (swarm_id, is_coordinator) = {
        let members = swarm_members.read().await;
        let swarm_id = members
            .get(req_session_id)
            .and_then(|member| member.swarm_id.clone());
        let is_coordinator = if let Some(ref swarm_id) = swarm_id {
            let coordinators = swarm_coordinators.read().await;
            coordinators
                .get(swarm_id)
                .map(|coordinator| coordinator == req_session_id)
                .unwrap_or(false)
        } else {
            false
        };
        (swarm_id, is_coordinator)
    };

    if !is_coordinator {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: permission_error.to_string(),
            retry_after_secs: None,
        });
        return None;
    }

    match swarm_id {
        Some(swarm_id) => Some(swarm_id),
        None => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: "Not in a swarm.".to_string(),
                retry_after_secs: None,
            });
            None
        }
    }
}
