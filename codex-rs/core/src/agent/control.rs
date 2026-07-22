use crate::agent::AgentStatus;
use crate::agent::registry::AgentMetadata;
use crate::agent::registry::AgentRegistry;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::resolve_role_config;
use crate::agent::status::is_final;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::config::Config;
use crate::config::RolloutBudgetConfig;
use crate::context_manager::is_user_turn_boundary;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::parse_turn_item;
use crate::rollout_budget::RolloutBudget;
use crate::session::emit_subagent_session_started;
use crate::session_prefix::format_inter_agent_completion_message;
use crate::session_prefix::format_subagent_context_line;
use crate::session_prefix::format_subagent_notification_message;
use crate::thread_manager::ResumeThreadWithHistoryOptions;
use crate::thread_manager::ThreadManagerState;
use crate::thread_rollout_truncation::truncate_rollout_to_last_n_fork_turns;
use codex_config::types::AgentWakeDescendantPolicy;
use codex_protocol::AgentPath;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ResumedHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::user_input::UserInput;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Weak;
use tokio::sync::Mutex;
use tokio::sync::watch;
use tracing::warn;

pub(crate) use self::execution::AgentExecutionGuard;
use self::execution::AgentExecutionLimiter;
use self::residency::V2Residency;

const ROOT_LAST_TASK_MESSAGE: &str = "Main thread";
pub(crate) const FORKED_SPAWN_AGENT_OUTPUT_MESSAGE: &str = "You are the newly spawned agent. The prior conversation history was forked from your parent agent. Treat the next user message as your new task, and use the forked history only as background context.";
mod execution;
mod legacy;
mod residency;
mod spawn;
mod wake;

use wake::CompletionWatcherMode;
use wake::ParentWakePreference;
use wake::ParentWakeSubscription;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpawnAgentForkMode {
    FullHistory,
    LastNTurns(usize),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnAgentOptions {
    pub(crate) fork_parent_spawn_call_id: Option<String>,
    pub(crate) fork_mode: Option<SpawnAgentForkMode>,
    pub(crate) wake_parent_on_completion: Option<bool>,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) environments: Option<Vec<TurnEnvironmentSelection>>,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveAgent {
    pub(crate) thread_id: ThreadId,
    pub(crate) metadata: AgentMetadata,
    pub(crate) status: AgentStatus,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ListedAgent {
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) agent_status: AgentStatus,
    pub(crate) last_task_message: Option<String>,
}

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is intended to be created at most once per root thread/session
/// tree. That same `AgentControl` is then shared with every sub-agent spawned from that root,
/// which keeps the registry scoped to that root thread rather than the entire `ThreadManager`.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// ID shared by the whole agent control session. This means every sub-agents from a common
    /// root share the same session ID.
    session_id: SessionId,
    /// Weak handle back to the global thread registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `ThreadManagerState -> CodexThread -> Session -> SessionServices -> ThreadManagerState`.
    manager: Weak<ThreadManagerState>,
    state: Arc<AgentRegistry>,
    parent_wake_subscriptions: Arc<Mutex<HashMap<ThreadId, ParentWakeSubscription>>>,
    parent_wake_preferences: Arc<Mutex<HashMap<ThreadId, ParentWakePreference>>>,
    v2_residency: Arc<V2Residency>,
    agent_execution_limiter: Arc<AgentExecutionLimiter>,
    /// Session-scoped state shared by the root thread and every cloned sub-agent control handle.
    rollout_budget: Arc<RolloutBudget>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(
        manager: Weak<ThreadManagerState>,
        rollout_budget: Option<RolloutBudgetConfig>,
    ) -> Self {
        let control = Self {
            manager,
            state: Arc::new(AgentRegistry::default()),
            parent_wake_subscriptions: Arc::new(Mutex::new(HashMap::new())),
            parent_wake_preferences: Arc::new(Mutex::new(HashMap::new())),
            ..Default::default()
        };
        if let Some(rollout_budget) = rollout_budget {
            control.rollout_budget.configure(rollout_budget);
        }
        control
    }

    pub(crate) fn with_session_id(mut self, session_id: SessionId, max_threads: usize) -> Self {
        self.session_id = session_id;
        self.agent_execution_limiter.initialize(max_threads);
        self
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn rollout_budget(&self) -> &RolloutBudget {
        self.rollout_budget.as_ref()
    }

    /// Send rich user input items to an existing agent thread.
    pub(crate) async fn send_input(
        &self,
        agent_id: ThreadId,
        input: Vec<UserInput>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, /*starts_turn*/ true)
            .await?;
        self.send_input_after_capacity_check(agent_id, &state, input)
            .await
    }

    async fn send_input_after_capacity_check(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        input: Vec<UserInput>,
    ) -> CodexResult<String> {
        let last_task_message = non_empty_task_message(render_input_preview(&input));
        let completion_watcher = self
            .maybe_prepare_completion_watcher_rearm(
                agent_id, /*trigger_turn*/ true,
                /*suppress_immediate_parent_notification*/ false,
            )
            .await;
        let result = self
            .handle_thread_request_result(
                agent_id,
                state,
                state.send_op(agent_id, input.into()).await,
            )
            .await;
        if result.is_ok() {
            match last_task_message {
                Some(last_task_message) => self
                    .state
                    .update_last_task_message(agent_id, last_task_message),
                None => self.state.clear_last_task_message(agent_id),
            }
            if let Some(completion_watcher) = completion_watcher {
                self.spawn_completion_watcher(
                    agent_id,
                    completion_watcher.parent_thread_id,
                    completion_watcher.child_reference,
                    completion_watcher.child_agent_path,
                    completion_watcher.completion_watcher_generation,
                    CompletionWatcherMode::NextStatusChangeThenTerminal,
                    Some(completion_watcher.status_rx),
                );
            }
        }
        result
    }

    pub(crate) async fn send_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        communication: InterAgentCommunication,
        agent_communication_context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, communication.trigger_turn)
            .await?;
        self.send_inter_agent_communication_after_capacity_check(
            agent_id,
            &state,
            communication,
            agent_communication_context,
        )
        .await
    }

    async fn send_inter_agent_communication_after_capacity_check(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        self.submit_inter_agent_communication(agent_id, state, communication, context)
            .await
    }

    async fn submit_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        let last_task_message = last_task_message_from_communication(&communication);
        let trigger_turn = communication.trigger_turn;
        let suppress_immediate_parent_notification = self
            .communication_reuses_child_from_descendant(agent_id, &communication)
            .await;
        let completion_watcher = self
            .maybe_prepare_completion_watcher_rearm(
                agent_id,
                trigger_turn,
                suppress_immediate_parent_notification,
            )
            .await;
        let communication_for_log =
            crate::agent_communication::logging_enabled().then(|| communication.clone());
        let result = self
            .handle_thread_request_result(
                agent_id,
                state,
                state
                    .send_op(agent_id, Op::InterAgentCommunication { communication })
                    .await,
            )
            .await;
        if let (Some(communication), Ok(communication_id)) =
            (communication_for_log, result.as_ref())
        {
            crate::agent_communication::emit_agent_communication_send(
                communication_id,
                &context,
                &communication,
                agent_id,
            );
        }
        if result.is_ok() {
            match last_task_message {
                Some(last_task_message) => self
                    .state
                    .update_last_task_message(agent_id, last_task_message),
                None => self.state.clear_last_task_message(agent_id),
            }
            if let Some(completion_watcher) = completion_watcher {
                self.spawn_completion_watcher(
                    agent_id,
                    completion_watcher.parent_thread_id,
                    completion_watcher.child_reference,
                    completion_watcher.child_agent_path,
                    completion_watcher.completion_watcher_generation,
                    CompletionWatcherMode::NextStatusChangeThenTerminal,
                    Some(completion_watcher.status_rx),
                );
            }
        }
        result
    }

    /// Interrupt the current task for an existing agent thread.
    pub(crate) async fn interrupt_agent(&self, agent_id: ThreadId) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.handle_thread_request_result(
            agent_id,
            &state,
            state.send_op(agent_id, Op::Interrupt).await,
        )
        .await
    }

    async fn handle_thread_request_result(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        result: CodexResult<String>,
    ) -> CodexResult<String> {
        if matches!(result, Err(CodexErr::InternalAgentDied)) {
            self.clear_parent_wake_state(agent_id).await;
            let _ = state.remove_thread(&agent_id).await;
            self.forget_v2_residency(agent_id);
            self.state.release_spawned_thread(agent_id);
        }
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: ThreadId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            // No agent available if upgrade fails.
            return AgentStatus::NotFound;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return AgentStatus::NotFound;
        };
        thread.agent_status().await
    }

    pub(crate) fn register_session_root(
        &self,
        current_thread_id: ThreadId,
        current_parent_thread_id: Option<ThreadId>,
    ) {
        if current_parent_thread_id.is_none() {
            self.state.register_root_thread(current_thread_id);
        }
    }

    pub(crate) fn register_live_thread_source(
        &self,
        thread_id: ThreadId,
        session_source: &SessionSource,
    ) {
        self.state
            .upsert_thread_spawn_source_metadata(thread_id, session_source);
    }

    pub(crate) fn get_agent_metadata(&self, agent_id: ThreadId) -> Option<AgentMetadata> {
        self.state.agent_metadata_for_thread(agent_id)
    }

    pub(crate) fn ensure_agent_known(&self, agent_id: ThreadId) -> CodexResult<AgentMetadata> {
        self.state
            .agent_metadata_for_thread(agent_id)
            .ok_or(CodexErr::ThreadNotFound(agent_id))
    }

    pub(crate) async fn list_live_agent_subtree_thread_ids(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut thread_ids = vec![agent_id];
        thread_ids.extend(self.live_thread_spawn_descendants(agent_id).await?);
        Ok(thread_ids)
    }

    pub(crate) async fn get_agent_config_snapshot(
        &self,
        agent_id: ThreadId,
    ) -> Option<ThreadConfigSnapshot> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return None;
        };
        Some(thread.config_snapshot().await)
    }

    pub(crate) async fn resolve_agent_reference(
        &self,
        current_thread_id: ThreadId,
        current_session_source: &SessionSource,
        agent_reference: &str,
    ) -> CodexResult<ThreadId> {
        let current_agent_path = current_session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let agent_path = current_agent_path
            .resolve(agent_reference)
            .map_err(CodexErr::UnsupportedOperation)?;
        let (root_thread_id, live_agents) = self
            .live_thread_spawn_agents_for_current_tree(current_thread_id, current_session_source)
            .await?;
        if agent_path.is_root() {
            return Ok(root_thread_id);
        }
        for (child_thread_id, metadata) in live_agents {
            if metadata.agent_path.as_ref() == Some(&agent_path) {
                return Ok(child_thread_id);
            }
        }
        Err(CodexErr::UnsupportedOperation(format!(
            "live agent path `{}` not found",
            agent_path.as_str()
        )))
    }

    pub(crate) async fn ensure_agent_target_in_current_tree(
        &self,
        current_thread_id: ThreadId,
        current_session_source: &SessionSource,
        agent_id: ThreadId,
    ) -> CodexResult<()> {
        let (root_thread_id, live_agents) = self
            .live_thread_spawn_agents_for_current_tree(current_thread_id, current_session_source)
            .await?;
        if agent_id == root_thread_id
            || live_agents
                .into_iter()
                .any(|(child_thread_id, _)| child_thread_id == agent_id)
        {
            return Ok(());
        }
        Err(CodexErr::UnsupportedOperation(format!(
            "agent id `{agent_id}` is not live in the current root thread tree"
        )))
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await?;
        Ok(thread.subscribe_status())
    }

    pub(crate) async fn format_environment_context_subagents(
        &self,
        parent_thread_id: ThreadId,
    ) -> String {
        let Ok(agents) = self.open_thread_spawn_children(parent_thread_id).await else {
            return String::new();
        };

        agents
            .into_iter()
            .map(|(thread_id, metadata)| {
                let reference = metadata
                    .agent_path
                    .as_ref()
                    .map(|agent_path| agent_path.name().to_string())
                    .unwrap_or_else(|| thread_id.to_string());
                format_subagent_context_line(reference.as_str(), metadata.agent_nickname.as_deref())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) async fn list_agents(
        &self,
        current_thread_id: ThreadId,
        current_session_source: &SessionSource,
        path_prefix: Option<&str>,
    ) -> CodexResult<Vec<ListedAgent>> {
        let state = self.upgrade()?;
        let resolved_prefix = path_prefix
            .map(|prefix| {
                current_session_source
                    .get_agent_path()
                    .unwrap_or_else(AgentPath::root)
                    .resolve(prefix)
                    .map_err(CodexErr::UnsupportedOperation)
            })
            .transpose()?;

        let (root_thread_id, mut live_agents) = self
            .live_thread_spawn_agents_for_current_tree(current_thread_id, current_session_source)
            .await?;
        live_agents.sort_by(|left, right| {
            left.1
                .agent_path
                .as_deref()
                .unwrap_or_default()
                .cmp(right.1.agent_path.as_deref().unwrap_or_default())
                .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
        });

        let root_path = AgentPath::root();
        let mut agents = Vec::with_capacity(live_agents.len().saturating_add(1));
        if resolved_prefix
            .as_ref()
            .is_none_or(|prefix| agent_matches_prefix(Some(&root_path), prefix))
            && let Ok(root_thread) = state.get_thread(root_thread_id).await
        {
            agents.push(ListedAgent {
                agent_id: root_thread_id.to_string(),
                agent_name: root_path.to_string(),
                agent_status: root_thread.agent_status().await,
                last_task_message: Some(ROOT_LAST_TASK_MESSAGE.to_string()),
            });
        }

        for (thread_id, metadata) in live_agents {
            if resolved_prefix
                .as_ref()
                .is_some_and(|prefix| !agent_matches_prefix(metadata.agent_path.as_ref(), prefix))
            {
                continue;
            }

            let Ok(thread) = state.get_thread(thread_id).await else {
                continue;
            };
            let agent_name = metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| thread_id.to_string());
            let agent_status = thread.agent_status().await;
            let last_task_message = if is_final(&agent_status) {
                None
            } else {
                match metadata.last_task_message.clone() {
                    Some(last_task_message) => Some(last_task_message),
                    None => last_task_message_for_thread(thread.as_ref()).await,
                }
            };
            agents.push(ListedAgent {
                agent_id: thread_id.to_string(),
                agent_name,
                agent_status,
                last_task_message,
            });
        }

        Ok(agents)
    }

    fn prepare_agent_metadata(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        parent_thread_id: Option<ThreadId>,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
    ) -> CodexResult<AgentMetadata> {
        if let Some(agent_path) = agent_path.as_ref() {
            reservation.reserve_agent_path(agent_path)?;
        }
        let candidate_names = spawn::agent_nickname_candidates(config, agent_role.as_deref());
        let candidate_name_refs: Vec<&str> = candidate_names.iter().map(String::as_str).collect();
        let agent_nickname = Some(reservation.reserve_agent_nickname_with_preference(
            &candidate_name_refs,
            preferred_agent_nickname.as_deref(),
        )?);
        Ok(AgentMetadata {
            agent_id: None,
            parent_thread_id,
            agent_path,
            agent_nickname,
            agent_role,
            last_task_message: None,
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn prepare_thread_spawn(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        parent_thread_id: ThreadId,
        depth: i32,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
    ) -> CodexResult<(SessionSource, AgentMetadata)> {
        if depth == 1 {
            self.state.register_root_thread(parent_thread_id);
        }
        let agent_metadata = self.prepare_agent_metadata(
            reservation,
            config,
            Some(parent_thread_id),
            agent_path,
            agent_role,
            preferred_agent_nickname,
        )?;
        let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_path: agent_metadata.agent_path.clone(),
            agent_nickname: agent_metadata.agent_nickname.clone(),
            agent_role: agent_metadata.agent_role.clone(),
        });
        Ok((session_source, agent_metadata))
    }

    fn upgrade(&self) -> CodexResult<Arc<ThreadManagerState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))
    }

    async fn inherited_environments_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
    ) -> Option<TurnEnvironmentSnapshot> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        Some(
            parent_thread
                .session
                .services
                .turn_environments
                .snapshot()
                .await,
        )
    }

    async fn inherited_exec_policy_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
        child_config: &Config,
    ) -> Option<Arc<crate::exec_policy::ExecPolicyManager>> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        let parent_config = parent_thread.session.get_config().await;
        if !crate::exec_policy::child_uses_parent_exec_policy(&parent_config, child_config) {
            return None;
        }

        Some(Arc::clone(&parent_thread.session.services.exec_policy))
    }

    async fn open_thread_spawn_children(
        &self,
        parent_thread_id: ThreadId,
    ) -> CodexResult<Vec<(ThreadId, AgentMetadata)>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        Ok(children_by_parent
            .remove(&parent_thread_id)
            .unwrap_or_default())
    }

    async fn live_thread_spawn_children(
        &self,
    ) -> CodexResult<HashMap<ThreadId, Vec<(ThreadId, AgentMetadata)>>> {
        let mut children_by_parent = self.state.live_thread_spawn_children_by_parent();

        // The registry has the freshest metadata for live agents (paths, nicknames, roles) and is
        // the fast path used by the agent picker/status UI. Fall back to the thread manager's live
        // spawn edges for any loaded child thread that has not been hydrated into the registry yet.
        if let Ok(state) = self.upgrade() {
            for (parent_thread_id, child_thread_id) in state.list_live_thread_spawn_edges().await {
                let metadata = match state.get_thread(child_thread_id).await {
                    Ok(thread) => self
                        .state
                        .agent_metadata_for_thread(child_thread_id)
                        .unwrap_or_else(|| {
                            agent_metadata_from_thread_spawn_source(
                                child_thread_id,
                                parent_thread_id,
                                &thread.session_source,
                            )
                        }),
                    Err(_) => self
                        .state
                        .agent_metadata_for_thread(child_thread_id)
                        .unwrap_or(AgentMetadata {
                            agent_id: Some(child_thread_id),
                            parent_thread_id: Some(parent_thread_id),
                            ..Default::default()
                        }),
                };
                let children = children_by_parent.entry(parent_thread_id).or_default();
                if children
                    .iter()
                    .any(|(existing_child_id, existing_metadata)| {
                        *existing_child_id == child_thread_id
                            || existing_metadata
                                .agent_path
                                .as_ref()
                                .zip(metadata.agent_path.as_ref())
                                .is_some_and(|(existing_path, candidate_path)| {
                                    existing_path == candidate_path
                                })
                    })
                {
                    continue;
                }
                children.push((child_thread_id, metadata));
            }
        }

        for children in children_by_parent.values_mut() {
            children.sort_by(|left, right| {
                left.1
                    .agent_path
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.1.agent_path.as_deref().unwrap_or_default())
                    .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
            });
        }

        Ok(children_by_parent)
    }

    async fn live_thread_spawn_agents_for_current_tree(
        &self,
        current_thread_id: ThreadId,
        current_session_source: &SessionSource,
    ) -> CodexResult<(ThreadId, Vec<(ThreadId, AgentMetadata)>)> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        let root_thread_id = current_root_thread_id(
            current_thread_id,
            current_session_source,
            &children_by_parent,
        );
        let descendants = remove_thread_spawn_descendants(&mut children_by_parent, root_thread_id);
        Ok((root_thread_id, descendants))
    }

    async fn persist_thread_spawn_edge_for_source(
        &self,
        child_thread: &crate::CodexThread,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
    ) {
        let Some(parent_thread_id) = session_source.and_then(SessionSource::parent_thread_id)
        else {
            return;
        };
        if child_thread.config_snapshot().await.ephemeral {
            return;
        }
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        if let Err(err) = agent_graph_store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                child_thread_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Open,
            )
            .await
        {
            warn!("failed to persist thread-spawn edge: {err}");
            return;
        }
        if let Some(agent_path) = session_source.and_then(SessionSource::get_agent_path) {
            let child_ids = match agent_graph_store
                .list_thread_spawn_children(
                    parent_thread_id,
                    Some(codex_agent_graph_store::ThreadSpawnEdgeStatus::Open),
                )
                .await
            {
                Ok(child_ids) => child_ids,
                Err(err) => {
                    warn!("failed to list open thread-spawn siblings for {agent_path}: {err}");
                    return;
                }
            };
            for sibling_thread_id in child_ids {
                if sibling_thread_id == child_thread_id {
                    continue;
                }
                let sibling = match state
                    .read_stored_thread(ReadThreadParams {
                        thread_id: sibling_thread_id,
                        include_archived: true,
                        include_history: false,
                    })
                    .await
                {
                    Ok(sibling) => sibling,
                    Err(err) => {
                        warn!("failed to read thread-spawn sibling {sibling_thread_id}: {err}");
                        continue;
                    }
                };
                if sibling.agent_path.as_deref() != Some(agent_path.as_str()) {
                    continue;
                }
                if let Err(err) = agent_graph_store
                    .set_thread_spawn_edge_status(
                        sibling_thread_id,
                        codex_agent_graph_store::ThreadSpawnEdgeStatus::Closed,
                    )
                    .await
                {
                    warn!(
                        "failed to close duplicate thread-spawn edge for {agent_path} sibling {sibling_thread_id}: {err}"
                    );
                }
            }
        }
    }

    async fn live_thread_spawn_descendants(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        Ok(
            remove_thread_spawn_descendants(&mut children_by_parent, root_thread_id)
                .into_iter()
                .map(|(thread_id, _)| thread_id)
                .collect(),
        )
    }
}

fn current_root_thread_id(
    current_thread_id: ThreadId,
    current_session_source: &SessionSource,
    children_by_parent: &HashMap<ThreadId, Vec<(ThreadId, AgentMetadata)>>,
) -> ThreadId {
    let parent_by_child = children_by_parent
        .iter()
        .flat_map(|(parent_thread_id, children)| {
            children
                .iter()
                .map(move |(child_thread_id, _)| (*child_thread_id, *parent_thread_id))
        })
        .collect::<HashMap<_, _>>();
    let mut root_thread_id = current_thread_id;
    if !parent_by_child.contains_key(&root_thread_id)
        && let Some(parent_thread_id) = current_session_source.parent_thread_id()
    {
        root_thread_id = parent_thread_id;
    }

    let mut seen = HashSet::new();
    while seen.insert(root_thread_id) {
        let Some(parent_thread_id) = parent_by_child.get(&root_thread_id) else {
            break;
        };
        root_thread_id = *parent_thread_id;
    }
    root_thread_id
}

fn remove_thread_spawn_descendants(
    children_by_parent: &mut HashMap<ThreadId, Vec<(ThreadId, AgentMetadata)>>,
    root_thread_id: ThreadId,
) -> Vec<(ThreadId, AgentMetadata)> {
    let mut descendants = Vec::new();
    let mut stack = children_by_parent
        .remove(&root_thread_id)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    while let Some((thread_id, metadata)) = stack.pop() {
        descendants.push((thread_id, metadata));
        if let Some(children) = children_by_parent.remove(&thread_id) {
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    descendants
}

fn agent_metadata_from_thread_spawn_source(
    thread_id: ThreadId,
    parent_thread_id: ThreadId,
    session_source: &SessionSource,
) -> AgentMetadata {
    let mut metadata = AgentMetadata {
        agent_id: Some(thread_id),
        parent_thread_id: Some(parent_thread_id),
        ..Default::default()
    };
    if let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        agent_path,
        agent_nickname,
        agent_role,
        ..
    }) = session_source
    {
        metadata.parent_thread_id = Some(*parent_thread_id);
        metadata.agent_path = agent_path.clone();
        metadata.agent_nickname = agent_nickname.clone();
        metadata.agent_role = agent_role.clone();
    }
    metadata
}

fn agent_matches_prefix(agent_path: Option<&AgentPath>, prefix: &AgentPath) -> bool {
    if prefix.is_root() {
        return true;
    }

    agent_path.is_some_and(|agent_path| {
        agent_path == prefix
            || agent_path
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

async fn last_task_message_for_thread(thread: &crate::CodexThread) -> Option<String> {
    let history = thread.session.clone_history().await;
    history
        .raw_items()
        .iter()
        .rev()
        .find_map(last_task_message_from_item)
}

fn last_task_message_from_item(item: &ResponseItem) -> Option<String> {
    if !is_user_turn_boundary(item) {
        return None;
    }

    match item {
        ResponseItem::Message { role, .. } if role == "user" => {
            let Some(TurnItem::UserMessage(message)) = parse_turn_item(item) else {
                return None;
            };
            Some(render_user_inputs(&message.content))
        }
        ResponseItem::Message { content, .. } => match content.as_slice() {
            [ContentItem::InputText { text }] | [ContentItem::OutputText { text }] => {
                serde_json::from_str::<InterAgentCommunication>(text)
                    .ok()
                    .map(|communication| communication.content)
            }
            _ => None,
        },
        _ => None,
    }
}

fn render_user_inputs(items: &[UserInput]) -> String {
    items
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path, .. } => format!("[local_image:{}]", path.display()),
            UserInput::Audio { .. } => "[audio]".to_string(),
            UserInput::LocalAudio { path } => {
                format!("[local_audio:{}]", path.display())
            }
            UserInput::Skill { name, path, .. } => format!("[skill:${name}]({})", path.display()),
            UserInput::Mention { name, path, .. } => format!("[mention:${name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_input_preview(input: &[UserInput]) -> String {
    render_user_inputs(input)
}

fn last_task_message_from_communication(communication: &InterAgentCommunication) -> Option<String> {
    if communication.encrypted_content.is_some() {
        return None;
    }
    non_empty_task_message(communication.content.clone())
}

fn non_empty_task_message(message: String) -> Option<String> {
    (!message.is_empty()).then_some(message)
}
fn thread_spawn_depth(session_source: &SessionSource) -> Option<i32> {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => Some(*depth),
        _ => None,
    }
}
#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
