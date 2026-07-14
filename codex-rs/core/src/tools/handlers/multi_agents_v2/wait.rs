use super::*;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::agent::status::is_final;
use crate::session::InputQueueActivity;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::multi_agents_spec::WaitAgentTimeoutOptions;
use crate::tools::handlers::multi_agents_spec::create_wait_agent_tool_v2;
use codex_config::types::AgentWaitOnWakeEnabledBehavior;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::CollabAgentRef;
use codex_tools::ToolSpec;
use futures::FutureExt;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch::Receiver;
use tokio::time::Instant;
use tokio::time::timeout_at;

#[derive(Default)]
pub(crate) struct Handler {
    options: WaitAgentTimeoutOptions,
}

impl Handler {
    pub(crate) fn new(options: WaitAgentTimeoutOptions) -> Self {
        Self { options }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("wait_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_wait_agent_tool_v2(self.options)
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl Handler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: WaitArgs = parse_arguments(&arguments)?;
        let min_timeout_ms = turn.config.multi_agent_v2.min_wait_timeout_ms;
        let max_timeout_ms = turn.config.multi_agent_v2.max_wait_timeout_ms;
        let default_timeout_ms = turn.config.multi_agent_v2.default_wait_timeout_ms;
        let timeout_ms = match args.timeout_ms {
            Some(ms) if ms < min_timeout_ms => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "timeout_ms must be at least {min_timeout_ms}"
                )));
            }
            Some(ms) if ms > max_timeout_ms => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "timeout_ms must be at most {max_timeout_ms}"
                )));
            }
            Some(ms) => ms,
            None => default_timeout_ms,
        };

        let receiver_thread_ids = resolve_agent_targets(&session, &turn, args.targets).await?;
        let mut receiver_agents = Vec::with_capacity(receiver_thread_ids.len());
        for receiver_thread_id in &receiver_thread_ids {
            let agent_metadata = session
                .services
                .agent_control
                .get_agent_metadata(*receiver_thread_id)
                .unwrap_or_default();
            receiver_agents.push(CollabAgentRef {
                thread_id: *receiver_thread_id,
                agent_nickname: agent_metadata.agent_nickname,
                agent_role: agent_metadata.agent_role,
            });
        }

        session
            .emit_turn_item_started(
                &turn,
                &TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                    id: call_id.clone(),
                    tool: CollabAgentTool::Wait,
                    status: CollabAgentToolCallStatus::InProgress,
                    sender_thread_id: session.thread_id,
                    receiver_thread_ids: receiver_thread_ids.clone(),
                    receiver_agents: receiver_agents.clone(),
                    prompt: None,
                    model: None,
                    reasoning_effort: None,
                    agents_states: Default::default(),
                }),
            )
            .await;

        let (outcome, statuses_by_id) = if receiver_thread_ids.is_empty() {
            let turn_state = session
                .input_queue
                .turn_state_for_sub_id(&session.active_turn, &turn.sub_id)
                .await;
            let (mut activity_rx, pending_activity) = session
                .input_queue
                .subscribe_activity(turn_state.as_deref())
                .await;
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            let outcome = wait_for_activity(&mut activity_rx, pending_activity, deadline).await;
            (outcome, HashMap::new())
        } else {
            let mut wake_enabled_children = session
                .services
                .agent_control
                .wake_enabled_children_for_parent(session.thread_id, &receiver_thread_ids)
                .await;
            let mut active_wake_enabled_children = Vec::with_capacity(wake_enabled_children.len());
            for child_thread_id in wake_enabled_children.drain(..) {
                let status = session
                    .services
                    .agent_control
                    .get_status(child_thread_id)
                    .await;
                if !is_final(&status) {
                    active_wake_enabled_children.push(child_thread_id);
                }
            }
            if !active_wake_enabled_children.is_empty()
                && matches!(
                    turn.config.agent_wait_on_wake_enabled_behavior,
                    AgentWaitOnWakeEnabledBehavior::Reject
                )
            {
                let ids = active_wake_enabled_children
                    .iter()
                    .map(ThreadId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(FunctionCallError::RespondToModel(format!(
                    "wait is disabled for wake-enabled child agents by current configuration. These child threads already have wake_parent_on_completion enabled for parent {}: {ids}. End the current turn and rely on the automatic wake path instead, or set agents.wait_on_wake_enabled = \"allow\" / spawn the child with wake_parent_on_completion=false when you explicitly want polling.",
                    session.thread_id
                )));
            }

            let mut status_rxs = Vec::with_capacity(receiver_thread_ids.len());
            let mut initial_final_statuses = Vec::new();
            for id in &receiver_thread_ids {
                match session.services.agent_control.subscribe_status(*id).await {
                    Ok(rx) => {
                        let status = rx.borrow().clone();
                        if is_final(&status) {
                            initial_final_statuses.push((*id, status));
                        }
                        status_rxs.push((*id, rx));
                    }
                    Err(CodexErr::ThreadNotFound(_)) => {
                        initial_final_statuses.push((*id, AgentStatus::NotFound));
                    }
                    Err(err) => {
                        let mut statuses = HashMap::with_capacity(1);
                        statuses.insert(*id, session.services.agent_control.get_status(*id).await);
                        session
                            .emit_turn_item_completed(
                                &turn,
                                TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                                    id: call_id.clone(),
                                    tool: CollabAgentTool::Wait,
                                    status: wait_tool_call_status(&statuses),
                                    sender_thread_id: session.thread_id,
                                    receiver_thread_ids: statuses.keys().copied().collect(),
                                    receiver_agents: wait_receiver_agents(
                                        &statuses,
                                        &receiver_agents,
                                    ),
                                    prompt: None,
                                    model: None,
                                    reasoning_effort: None,
                                    agents_states: statuses,
                                }),
                            )
                            .await;
                        return Err(collab_agent_error(*id, err));
                    }
                }
            }

            let statuses = if !initial_final_statuses.is_empty() {
                initial_final_statuses
            } else {
                let mut futures = FuturesUnordered::new();
                for (id, rx) in status_rxs {
                    let session = session.clone();
                    futures.push(wait_for_final_status(session, id, rx));
                }
                let mut results = Vec::new();
                let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
                loop {
                    match timeout_at(deadline, futures.next()).await {
                        Ok(Some(Some(result))) => {
                            results.push(result);
                            break;
                        }
                        Ok(Some(None)) => continue,
                        Ok(None) | Err(_) => break,
                    }
                }
                if !results.is_empty() {
                    loop {
                        match futures.next().now_or_never() {
                            Some(Some(Some(result))) => results.push(result),
                            Some(Some(None)) => continue,
                            Some(None) | None => break,
                        }
                    }
                }
                results
            };

            let outcome = if statuses.is_empty() {
                WaitOutcome::TimedOut
            } else {
                WaitOutcome::MailboxActivity
            };
            let statuses_by_id = statuses.into_iter().collect::<HashMap<_, _>>();
            (outcome, statuses_by_id)
        };
        let result = WaitAgentResult::from_outcome(outcome);

        session
            .emit_turn_item_completed(
                &turn,
                TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                    id: call_id.clone(),
                    tool: CollabAgentTool::Wait,
                    status: wait_tool_call_status(&statuses_by_id),
                    sender_thread_id: session.thread_id,
                    receiver_thread_ids: statuses_by_id.keys().copied().collect(),
                    receiver_agents: wait_receiver_agents(&statuses_by_id, &receiver_agents),
                    prompt: None,
                    model: None,
                    reasoning_effort: None,
                    agents_states: statuses_by_id,
                }),
            )
            .await;

        Ok(boxed_tool_output(result))
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    #[serde(default)]
    targets: Vec<String>,
    timeout_ms: Option<i64>,
}

async fn resolve_agent_targets(
    session: &std::sync::Arc<Session>,
    turn: &std::sync::Arc<TurnContext>,
    targets: Vec<String>,
) -> Result<Vec<ThreadId>, FunctionCallError> {
    let mut receiver_thread_ids = Vec::with_capacity(targets.len());
    for target in targets {
        receiver_thread_ids.push(resolve_agent_target(session, turn, &target).await?);
    }
    Ok(receiver_thread_ids)
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WaitAgentResult {
    pub(crate) message: String,
    pub(crate) timed_out: bool,
}

impl WaitAgentResult {
    fn from_outcome(outcome: WaitOutcome) -> Self {
        let message = match outcome {
            WaitOutcome::MailboxActivity => "Wait completed.",
            WaitOutcome::Steered => "Wait interrupted by new input.",
            WaitOutcome::TimedOut => "Wait timed out.",
        };
        Self {
            message: message.to_string(),
            timed_out: outcome == WaitOutcome::TimedOut,
        }
    }
}

impl ToolOutput for WaitAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "wait_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, /*success*/ None, "wait_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "wait_agent")
    }
}

async fn wait_for_final_status(
    session: std::sync::Arc<Session>,
    thread_id: ThreadId,
    mut status_rx: Receiver<AgentStatus>,
) -> Option<(ThreadId, AgentStatus)> {
    let mut status = status_rx.borrow().clone();
    if is_final(&status) {
        return Some((thread_id, status));
    }

    loop {
        if status_rx.changed().await.is_err() {
            let latest = session.services.agent_control.get_status(thread_id).await;
            return is_final(&latest).then_some((thread_id, latest));
        }
        status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some((thread_id, status));
        }
    }
}

fn wait_tool_call_status(statuses: &HashMap<ThreadId, AgentStatus>) -> CollabAgentToolCallStatus {
    if statuses
        .values()
        .any(|status| matches!(status, AgentStatus::Errored(_) | AgentStatus::NotFound))
    {
        CollabAgentToolCallStatus::Failed
    } else {
        CollabAgentToolCallStatus::Completed
    }
}

fn wait_receiver_agents(
    statuses: &HashMap<ThreadId, AgentStatus>,
    receiver_agents: &[CollabAgentRef],
) -> Vec<CollabAgentRef> {
    if statuses.is_empty() {
        return Vec::new();
    }

    receiver_agents
        .iter()
        .filter(|agent| statuses.contains_key(&agent.thread_id))
        .cloned()
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
    MailboxActivity,
    Steered,
    TimedOut,
}

async fn wait_for_activity(
    activity_rx: &mut tokio::sync::watch::Receiver<InputQueueActivity>,
    pending_activity: Option<InputQueueActivity>,
    deadline: Instant,
) -> WaitOutcome {
    if let Some(activity) = pending_activity {
        return match activity {
            InputQueueActivity::Mailbox => WaitOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitOutcome::Steered,
        };
    }

    match timeout_at(deadline, activity_rx.changed()).await {
        Ok(Ok(())) => match *activity_rx.borrow_and_update() {
            InputQueueActivity::Mailbox => WaitOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitOutcome::Steered,
        },
        Ok(Err(_)) | Err(_) => WaitOutcome::TimedOut,
    }
}
