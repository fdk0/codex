//! Shared argument parsing and dispatch for the v2 agent messaging tools.
//!
//! `send_message` and `followup_task` share the same submission path and differ only in whether the
//! resulting `InterAgentCommunication` should wake the target immediately.

use super::*;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::context::FunctionToolOutput;
use codex_protocol::protocol::InterAgentCommunication;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageDeliveryMode {
    QueueOnly,
    TriggerTurn,
}

impl MessageDeliveryMode {
    /// Returns whether the produced communication should start a turn immediately.
    fn apply(self, communication: InterAgentCommunication) -> InterAgentCommunication {
        match self {
            Self::QueueOnly => InterAgentCommunication {
                trigger_turn: false,
                ..communication
            },
            Self::TriggerTurn => InterAgentCommunication {
                trigger_turn: true,
                ..communication
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Input for the MultiAgentV2 `send_message` tool.
pub(crate) struct SendMessageArgs {
    pub(crate) target: String,
    pub(crate) message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Input for the MultiAgentV2 `followup_task` tool.
pub(crate) struct FollowupTaskArgs {
    pub(crate) target: String,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) interrupt: bool,
}

pub(super) fn message_content(message: String) -> Result<String, FunctionCallError> {
    if message.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "Empty message can't be sent to an agent".to_string(),
        ));
    }
    Ok(message)
}

/// Handles the shared MultiAgentV2 message flow for both `send_message` and `followup_task`.
pub(crate) async fn handle_message_string_tool(
    invocation: ToolInvocation,
    mode: MessageDeliveryMode,
    target: String,
    message: String,
    interrupt: bool,
) -> Result<FunctionToolOutput, FunctionCallError> {
    let message = message_content(message)?;
    let ToolInvocation {
        session,
        turn,
        call_id,
        ..
    } = invocation;
    let receiver_thread_id = resolve_agent_target(&session, &turn, &target).await?;
    let receiver_agent = session
        .services
        .agent_control
        .ensure_agent_known(receiver_thread_id)
        .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
    if mode == MessageDeliveryMode::TriggerTurn
        && receiver_agent
            .agent_path
            .as_ref()
            .is_some_and(AgentPath::is_root)
    {
        return Err(FunctionCallError::RespondToModel(
            "Follow-up tasks can't target the root agent".to_string(),
        ));
    }
    let receiver_agent_path = receiver_agent.agent_path.clone().ok_or_else(|| {
        FunctionCallError::RespondToModel("target agent is missing an agent_path".to_string())
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    validate_message_target_lineage(mode, &author, &receiver_agent_path)
        .map_err(FunctionCallError::RespondToModel)?;
    let resume_config = build_agent_resume_config(turn.as_ref())?;
    session
        .services
        .agent_control
        .ensure_v2_agent_loaded(resume_config, receiver_thread_id)
        .await
        .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
    if interrupt {
        session
            .services
            .agent_control
            .interrupt_agent(receiver_thread_id)
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
    }
    let communication = communication_from_tool_message(
        author,
        receiver_agent_path.clone(),
        message,
        turn.config.multi_agent_v2.encrypted_messages,
    );
    let kind = match mode {
        MessageDeliveryMode::QueueOnly => AgentCommunicationKind::Message,
        MessageDeliveryMode::TriggerTurn => AgentCommunicationKind::Followup,
    };
    let context = AgentCommunicationContext::new(kind, session.thread_id);
    let result = session
        .services
        .agent_control
        .send_inter_agent_communication(receiver_thread_id, mode.apply(communication), context)
        .await
        .map_err(|err| collab_agent_error(receiver_thread_id, err));
    result?;
    emit_sub_agent_activity(
        &session,
        &turn,
        SubAgentActivityItem {
            id: call_id,
            agent_thread_id: receiver_thread_id,
            agent_path: receiver_agent_path,
            kind: SubAgentActivityKind::Interacted,
        },
    )
    .await;

    Ok(FunctionToolOutput::from_text(String::new(), Some(true)))
}

fn validate_message_target_lineage(
    mode: MessageDeliveryMode,
    author: &AgentPath,
    receiver: &AgentPath,
) -> Result<(), String> {
    if author.is_root()
        || agent_path_has_prefix(receiver, author)
        || (mode == MessageDeliveryMode::QueueOnly && agent_path_has_prefix(author, receiver))
    {
        return Ok(());
    }

    Err(format!(
        "target agent `{receiver}` is outside the current agent lineage `{author}`; subagents can only message agents in their descendant subtree, or queue messages to an ancestor"
    ))
}

fn agent_path_has_prefix(agent_path: &AgentPath, prefix: &AgentPath) -> bool {
    agent_path == prefix
        || agent_path
            .as_str()
            .strip_prefix(prefix.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn root_can_message_any_agent_lineage() {
        let root = AgentPath::root();
        let receiver = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(MessageDeliveryMode::TriggerTurn, &root, &receiver),
            Ok(())
        );
    }

    #[test]
    fn subagent_can_message_own_descendant_lineage() {
        let dispatcher = AgentPath::try_from("/root/dispatcher").expect("valid path");
        let review = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(MessageDeliveryMode::TriggerTurn, &dispatcher, &review),
            Ok(())
        );
    }

    #[test]
    fn subagent_can_queue_message_to_ancestor_lineage() {
        let dispatcher = AgentPath::try_from("/root/dispatcher").expect("valid path");
        let review = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(MessageDeliveryMode::QueueOnly, &review, &dispatcher),
            Ok(())
        );
    }

    #[test]
    fn subagent_cannot_trigger_ancestor_lineage() {
        let dispatcher = AgentPath::try_from("/root/dispatcher").expect("valid path");
        let review = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(MessageDeliveryMode::TriggerTurn, &review, &dispatcher),
            Err(
                "target agent `/root/dispatcher` is outside the current agent lineage `/root/dispatcher/review`; subagents can only message agents in their descendant subtree, or queue messages to an ancestor"
                    .to_string()
            )
        );
    }

    #[test]
    fn subagent_cannot_message_sibling_descendant_lineage() {
        let replacement = AgentPath::try_from("/root/dispatcher_b").expect("valid path");
        let old_review = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(
                MessageDeliveryMode::TriggerTurn,
                &replacement,
                &old_review
            ),
            Err(
                "target agent `/root/dispatcher/review` is outside the current agent lineage `/root/dispatcher_b`; subagents can only message agents in their descendant subtree, or queue messages to an ancestor"
                    .to_string()
            )
        );
    }

    #[test]
    fn subagent_cannot_queue_message_to_sibling_descendant_lineage() {
        let replacement = AgentPath::try_from("/root/dispatcher_b").expect("valid path");
        let old_review = AgentPath::try_from("/root/dispatcher/review").expect("valid path");

        assert_eq!(
            validate_message_target_lineage(MessageDeliveryMode::QueueOnly, &replacement, &old_review),
            Err(
                "target agent `/root/dispatcher/review` is outside the current agent lineage `/root/dispatcher_b`; subagents can only message agents in their descendant subtree, or queue messages to an ancestor"
                    .to_string()
            )
        );
    }
}
