use super::*;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub(super) struct ParentWakeSubscription {
    pub(super) parent_thread_id: ThreadId,
    pub(super) wake_parent_on_completion: bool,
    pub(super) wake_descendant_policy: AgentWakeDescendantPolicy,
    pub(super) child_reference: String,
    pub(super) child_agent_path: Option<AgentPath>,
    pub(super) completion_watcher_generation: u64,
    pub(super) last_notified_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ParentWakePreference {
    pub(super) wake_parent_on_completion: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CompletionWatcherMode {
    CurrentOrNextTerminal,
    NextStatusChangeThenTerminal,
}

#[derive(Debug)]
pub(super) struct CompletionWatcherArm {
    pub(super) parent_thread_id: ThreadId,
    pub(super) child_reference: String,
    pub(super) child_agent_path: Option<AgentPath>,
    pub(super) completion_watcher_generation: u64,
    pub(super) status_rx: watch::Receiver<AgentStatus>,
}

impl AgentControl {
    /// Starts a detached watcher for sub-agents spawned from another thread.
    ///
    /// This is only enabled for `SubAgentSource::ThreadSpawn`, where a parent thread exists and
    /// can receive completion notifications.
    pub(super) async fn maybe_start_completion_watcher(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<SessionSource>,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
        mode: CompletionWatcherMode,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return;
        };
        self.parent_wake_subscriptions
            .lock()
            .await
            .entry(child_thread_id)
            .or_insert_with(|| ParentWakeSubscription {
                parent_thread_id,
                wake_parent_on_completion: false,
                wake_descendant_policy: AgentWakeDescendantPolicy::Immediate,
                child_reference: child_reference.clone(),
                child_agent_path: child_agent_path.clone(),
                completion_watcher_generation: 0,
                last_notified_generation: None,
            });
        self.parent_wake_preferences
            .lock()
            .await
            .entry(child_thread_id)
            .or_insert(ParentWakePreference {
                wake_parent_on_completion: false,
            });
        self.spawn_completion_watcher(
            child_thread_id,
            parent_thread_id,
            child_reference,
            child_agent_path,
            /*completion_watcher_generation*/ 0,
            mode,
            /*status_rx*/ None,
        );
    }

    pub(super) async fn maybe_prepare_completion_watcher_rearm(
        &self,
        child_thread_id: ThreadId,
        trigger_turn: bool,
        suppress_immediate_parent_notification: bool,
    ) -> Option<CompletionWatcherArm> {
        if !trigger_turn {
            return None;
        }

        let mut status_rx = self.subscribe_status(child_thread_id).await.ok()?;
        let current_status = status_rx.borrow().clone();
        let current_status_is_final = is_final(&current_status);
        let current_status_allows_rearm =
            current_status_is_final || matches!(current_status, AgentStatus::PendingInit);
        if current_status_is_final {
            let _ = status_rx.borrow_and_update();
        }

        let wake_descendant_policy = self.read_wake_descendant_policy(child_thread_id).await;
        let (
            previous_generation,
            child_agent_path,
            child_reference,
            completion_watcher_generation,
            parent_thread_id,
        ) = {
            let mut subscriptions = self.parent_wake_subscriptions.lock().await;
            let subscription = subscriptions.get_mut(&child_thread_id)?;
            if let Some(wake_descendant_policy) = wake_descendant_policy {
                subscription.wake_descendant_policy = wake_descendant_policy;
            }
            let previous_generation = subscription.completion_watcher_generation;
            if !current_status_allows_rearm
                && subscription.last_notified_generation != Some(previous_generation)
            {
                return None;
            }
            let child_agent_path = subscription.child_agent_path.clone();
            let child_reference = subscription.child_reference.clone();
            subscription.completion_watcher_generation =
                subscription.completion_watcher_generation.saturating_add(1);
            let completion_watcher_generation = subscription.completion_watcher_generation;
            let parent_thread_id = subscription.parent_thread_id;
            (
                previous_generation,
                child_agent_path,
                child_reference,
                completion_watcher_generation,
                parent_thread_id,
            )
        };

        if current_status_is_final
            && !suppress_immediate_parent_notification
            && !self.leaf_only_wake_blocked(child_thread_id).await
        {
            self.notify_completion_to_parent(
                child_thread_id,
                parent_thread_id,
                child_reference.clone(),
                child_agent_path.clone(),
                previous_generation,
                current_status,
            )
            .await;
        }

        Some(CompletionWatcherArm {
            parent_thread_id,
            child_reference,
            child_agent_path,
            completion_watcher_generation,
            status_rx,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_completion_watcher(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
        completion_watcher_generation: u64,
        mode: CompletionWatcherMode,
        status_rx: Option<watch::Receiver<AgentStatus>>,
    ) {
        let control = self.clone();
        tokio::spawn(async move {
            let status_rx = match status_rx {
                Some(status_rx) => status_rx,
                None => match control.subscribe_status(child_thread_id).await {
                    Ok(status_rx) => status_rx,
                    Err(_) => {
                        control
                            .retry_notify_completion_to_parent(
                                child_thread_id,
                                parent_thread_id,
                                child_reference,
                                child_agent_path,
                                completion_watcher_generation,
                                control.get_status(child_thread_id).await,
                            )
                            .await;
                        return;
                    }
                },
            };
            let Some(status) = control
                .wait_for_completion_status(
                    child_thread_id,
                    status_rx,
                    mode,
                    completion_watcher_generation,
                )
                .await
            else {
                return;
            };
            if !is_final(&status) {
                return;
            }
            if control
                .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                .await
            {
                return;
            }
            control
                .retry_notify_completion_to_parent(
                    child_thread_id,
                    parent_thread_id,
                    child_reference,
                    child_agent_path,
                    completion_watcher_generation,
                    status,
                )
                .await;
        });
    }

    async fn wait_for_completion_status(
        &self,
        child_thread_id: ThreadId,
        mut status_rx: watch::Receiver<AgentStatus>,
        mode: CompletionWatcherMode,
        completion_watcher_generation: u64,
    ) -> Option<AgentStatus> {
        let mut status = status_rx.borrow().clone();
        if matches!(mode, CompletionWatcherMode::NextStatusChangeThenTerminal) && is_final(&status)
        {
            if status_rx.changed().await.is_err() {
                return if self
                    .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                    .await
                {
                    None
                } else {
                    Some(self.get_status(child_thread_id).await)
                };
            }
            status = status_rx.borrow().clone();
        }

        loop {
            if self
                .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                .await
            {
                return None;
            }
            while !is_final(&status) {
                if status_rx.changed().await.is_err() {
                    return if self
                        .completion_watcher_superseded(
                            child_thread_id,
                            completion_watcher_generation,
                        )
                        .await
                    {
                        None
                    } else {
                        Some(self.get_status(child_thread_id).await)
                    };
                }
                status = status_rx.borrow().clone();
                if self
                    .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                    .await
                {
                    return None;
                }
            }
            if !self.leaf_only_wake_blocked(child_thread_id).await {
                return Some(status);
            }
            let next_status = self
                .wait_for_wakeable_leaf_only_status(
                    child_thread_id,
                    &mut status_rx,
                    completion_watcher_generation,
                )
                .await?;
            status = next_status;
        }
    }

    async fn retry_notify_completion_to_parent(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
        completion_watcher_generation: u64,
        status: AgentStatus,
    ) {
        loop {
            if !is_final(&status)
                || self
                    .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                    .await
            {
                return;
            }
            if self
                .notify_completion_to_parent(
                    child_thread_id,
                    parent_thread_id,
                    child_reference.clone(),
                    child_agent_path.clone(),
                    completion_watcher_generation,
                    status.clone(),
                )
                .await
            {
                return;
            }
            sleep(Duration::from_millis(250)).await;
        }
    }

    async fn notify_completion_to_parent(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
        completion_watcher_generation: u64,
        status: AgentStatus,
    ) -> bool {
        if !is_final(&status) {
            return true;
        }

        let wake_parent_on_completion = {
            let subscriptions = self.parent_wake_subscriptions.lock().await;
            let Some(subscription) = subscriptions.get(&child_thread_id) else {
                return true;
            };
            if subscription.completion_watcher_generation != completion_watcher_generation
                || subscription.last_notified_generation == Some(completion_watcher_generation)
            {
                return true;
            }
            subscription.wake_parent_on_completion
        };

        let delivered = if let Ok(state) = self.upgrade() {
            let child_thread = state.get_thread(child_thread_id).await.ok();
            if child_agent_path.is_some()
                && child_thread
                    .as_ref()
                    .map(|thread| thread.multi_agent_version() == Some(MultiAgentVersion::V2))
                    .unwrap_or(true)
            {
                let delivery_paths = child_agent_path.clone().and_then(|child_agent_path| {
                    let parent_agent_path = child_agent_path
                        .as_str()
                        .rsplit_once('/')
                        .and_then(|(parent, _)| AgentPath::try_from(parent).ok())?;
                    Some((child_agent_path, parent_agent_path))
                });
                if let Some((child_agent_path, parent_agent_path)) = delivery_paths {
                    let Some(message) = format_inter_agent_completion_message(
                        parent_agent_path.clone(),
                        child_agent_path.clone(),
                        &status,
                    ) else {
                        return true;
                    };
                    if !self
                        .ensure_completion_parent_loaded(parent_thread_id, child_thread.as_deref())
                        .await
                    {
                        return false;
                    }
                    let communication = InterAgentCommunication::new(
                        child_agent_path,
                        parent_agent_path,
                        Vec::new(),
                        message,
                        wake_parent_on_completion,
                    );
                    let context = AgentCommunicationContext::new(
                        AgentCommunicationKind::Result,
                        child_thread_id,
                    );
                    self.send_inter_agent_communication_boxed(
                        parent_thread_id,
                        communication,
                        context,
                    )
                    .await
                    .is_ok()
                } else {
                    true
                }
            } else {
                let message =
                    format_subagent_notification_message(child_reference.as_str(), &status);
                if !self
                    .ensure_completion_parent_loaded(parent_thread_id, child_thread.as_deref())
                    .await
                {
                    return false;
                }
                self.notify_parent_with_contextual_message(
                    parent_thread_id,
                    message,
                    wake_parent_on_completion,
                )
                .await
            }
        } else {
            false
        };

        if !delivered {
            return false;
        }

        let mut subscriptions = self.parent_wake_subscriptions.lock().await;
        let Some(subscription) = subscriptions.get_mut(&child_thread_id) else {
            return true;
        };
        if subscription.completion_watcher_generation != completion_watcher_generation {
            return true;
        }
        subscription.last_notified_generation = Some(completion_watcher_generation);
        true
    }

    async fn ensure_completion_parent_loaded(
        &self,
        parent_thread_id: ThreadId,
        child_thread: Option<&crate::CodexThread>,
    ) -> bool {
        let Ok(state) = self.upgrade() else {
            return false;
        };
        if state.get_thread(parent_thread_id).await.is_ok() {
            return true;
        }

        let Some(child_thread) = child_thread else {
            return false;
        };
        let config = child_thread
            .codex
            .session
            .get_config()
            .await
            .as_ref()
            .clone();
        match self.ensure_v2_agent_loaded(config, parent_thread_id).await {
            Ok(()) => true,
            Err(err) => {
                warn!(
                    "failed to reload parent thread {parent_thread_id} for child completion wake: {err}"
                );
                false
            }
        }
    }

    async fn leaf_only_wake_blocked(&self, child_thread_id: ThreadId) -> bool {
        matches!(
            self.parent_wake_subscriptions
                .lock()
                .await
                .get(&child_thread_id)
                .map(|subscription| subscription.wake_descendant_policy),
            Some(AgentWakeDescendantPolicy::LeafOnly)
        ) && self.has_active_descendants(child_thread_id).await
    }

    async fn wait_for_wakeable_leaf_only_status(
        &self,
        child_thread_id: ThreadId,
        status_rx: &mut watch::Receiver<AgentStatus>,
        completion_watcher_generation: u64,
    ) -> Option<AgentStatus> {
        loop {
            if self
                .completion_watcher_superseded(child_thread_id, completion_watcher_generation)
                .await
            {
                return None;
            }
            if !self.leaf_only_wake_blocked(child_thread_id).await {
                let status = self.get_status(child_thread_id).await;
                if !is_final(&status) {
                    return Some(status);
                }
            }

            tokio::select! {
                changed = status_rx.changed() => {
                    if changed.is_err() {
                        let status = self.get_status(child_thread_id).await;
                        return Some(status);
                    }
                    let status = status_rx.borrow().clone();
                    if self
                        .completion_watcher_superseded(
                            child_thread_id,
                            completion_watcher_generation,
                        )
                        .await
                    {
                        return None;
                    }
                    if !is_final(&status) {
                        return Some(status);
                    }
                    if !self.leaf_only_wake_blocked(child_thread_id).await
                        && !self
                            .child_has_scheduled_or_active_turn(child_thread_id)
                            .await
                    {
                        return Some(status);
                    }
                }
                _ = sleep(Duration::from_millis(25)) => {
                    if !self.leaf_only_wake_blocked(child_thread_id).await {
                        let status = self.get_status(child_thread_id).await;
                        if !is_final(&status)
                            || !self
                                .child_has_scheduled_or_active_turn(child_thread_id)
                                .await
                        {
                            return Some(status);
                        }
                    }
                }
            }
        }
    }

    async fn child_has_scheduled_or_active_turn(&self, child_thread_id: ThreadId) -> bool {
        let Ok(state) = self.upgrade() else {
            return false;
        };
        let Ok(child_thread) = state.get_thread(child_thread_id).await else {
            return false;
        };
        if child_thread
            .codex
            .session
            .active_turn
            .lock()
            .await
            .is_some()
        {
            return true;
        }
        child_thread
            .codex
            .session
            .input_queue
            .has_trigger_turn_mailbox_items()
            .await
            || child_thread
                .codex
                .session
                .input_queue
                .has_pending_input(&child_thread.codex.session.active_turn)
                .await
    }

    pub(super) async fn communication_reuses_child_from_descendant(
        &self,
        child_thread_id: ThreadId,
        communication: &InterAgentCommunication,
    ) -> bool {
        let Some(child_agent_path) = self
            .parent_wake_subscriptions
            .lock()
            .await
            .get(&child_thread_id)
            .and_then(|subscription| subscription.child_agent_path.clone())
        else {
            return false;
        };
        agent_matches_prefix(Some(&communication.author), &child_agent_path)
            && communication.author != child_agent_path
    }

    pub(super) async fn register_parent_wake_subscription(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
        wake_parent_on_completion: bool,
        wake_descendant_policy: AgentWakeDescendantPolicy,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return;
        };
        let child_agent_path = match session_source {
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. })) => {
                agent_path.clone()
            }
            _ => None,
        };
        let child_reference = child_agent_path
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| child_thread_id.to_string());
        self.parent_wake_preferences.lock().await.insert(
            child_thread_id,
            ParentWakePreference {
                wake_parent_on_completion,
            },
        );
        self.parent_wake_subscriptions.lock().await.insert(
            child_thread_id,
            ParentWakeSubscription {
                parent_thread_id: *parent_thread_id,
                wake_parent_on_completion,
                wake_descendant_policy,
                child_reference,
                child_agent_path,
                completion_watcher_generation: 0,
                last_notified_generation: None,
            },
        );
    }

    pub(crate) async fn restore_parent_wake_subscription_for_loaded_thread(
        &self,
        child_thread_id: ThreadId,
        session_source: &SessionSource,
        config: &Config,
    ) {
        let wake_parent_on_completion = self
            .wake_parent_on_completion_for_thread(
                child_thread_id,
                Some(session_source),
                config.agent_wake_parent_on_completion_default,
            )
            .await;
        self.register_parent_wake_subscription(
            child_thread_id,
            Some(session_source),
            wake_parent_on_completion,
            config.agent_wake_descendant_policy,
        )
        .await;
    }

    pub(super) async fn wake_parent_on_completion_for_thread(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
        fallback_default: bool,
    ) -> bool {
        if let Some(preference) = self
            .parent_wake_preferences
            .lock()
            .await
            .get(&child_thread_id)
            .copied()
        {
            return preference.wake_parent_on_completion;
        }

        matches!(
            session_source,
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. }))
        ) && fallback_default
    }

    pub(super) async fn clear_parent_wake_state(&self, child_thread_id: ThreadId) {
        self.parent_wake_subscriptions
            .lock()
            .await
            .remove(&child_thread_id);
        self.parent_wake_preferences
            .lock()
            .await
            .remove(&child_thread_id);
    }

    async fn completion_watcher_superseded(
        &self,
        child_thread_id: ThreadId,
        completion_watcher_generation: u64,
    ) -> bool {
        self.parent_wake_subscriptions
            .lock()
            .await
            .get(&child_thread_id)
            .map(|subscription| {
                subscription.completion_watcher_generation != completion_watcher_generation
            })
            .unwrap_or(true)
    }

    fn send_inter_agent_communication_boxed(
        &self,
        agent_id: ThreadId,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
    ) -> futures::future::BoxFuture<'_, CodexResult<String>> {
        Box::pin(self.send_inter_agent_communication(agent_id, communication, context))
    }

    pub(crate) async fn wake_enabled_children_for_parent(
        &self,
        parent_thread_id: ThreadId,
        child_thread_ids: &[ThreadId],
    ) -> Vec<ThreadId> {
        let subscriptions = self.parent_wake_subscriptions.lock().await;
        child_thread_ids
            .iter()
            .copied()
            .filter(|child_thread_id| {
                subscriptions
                    .get(child_thread_id)
                    .is_some_and(|subscription| {
                        subscription.parent_thread_id == parent_thread_id
                            && subscription.wake_parent_on_completion
                    })
            })
            .collect()
    }

    pub(crate) async fn has_pending_wake_enabled_children_for_parent(
        &self,
        parent_thread_id: ThreadId,
    ) -> bool {
        self.parent_wake_subscriptions
            .lock()
            .await
            .values()
            .any(|subscription| {
                subscription.parent_thread_id == parent_thread_id
                    && subscription.wake_parent_on_completion
                    && subscription.last_notified_generation
                        != Some(subscription.completion_watcher_generation)
            })
    }

    async fn notify_parent_with_contextual_message(
        &self,
        parent_thread_id: ThreadId,
        message: String,
        trigger_turn: bool,
    ) -> bool {
        let Ok(state) = self.upgrade() else {
            return false;
        };
        let Ok(parent_thread) = state.get_thread(parent_thread_id).await else {
            return false;
        };
        if !trigger_turn {
            parent_thread
                .inject_user_message_without_turn(message)
                .await;
            return true;
        }

        let pending_item = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: message }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if parent_thread
            .inject_if_running(vec![pending_item.clone()])
            .await
            .is_ok()
        {
            return true;
        }

        if let Err(err) = parent_thread
            .try_start_turn_if_idle(vec![pending_item])
            .await
        {
            let _ = parent_thread.inject_response_items(err.into_input()).await;
        }
        true
    }

    async fn has_active_descendants(&self, owner_thread_id: ThreadId) -> bool {
        let Ok(descendants) = self.live_thread_spawn_descendants(owner_thread_id).await else {
            return false;
        };
        for descendant_id in descendants {
            if !is_final(&self.get_status(descendant_id).await)
                || self.child_has_scheduled_or_active_turn(descendant_id).await
            {
                return true;
            }
        }
        false
    }

    async fn read_wake_descendant_policy(
        &self,
        child_thread_id: ThreadId,
    ) -> Option<AgentWakeDescendantPolicy> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(child_thread) = state.get_thread(child_thread_id).await else {
            return None;
        };
        Some(
            child_thread
                .config_snapshot()
                .await
                .agent_wake_descendant_policy,
        )
    }
}
