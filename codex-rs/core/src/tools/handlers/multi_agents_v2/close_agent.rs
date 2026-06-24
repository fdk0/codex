use super::*;
use crate::tools::handlers::multi_agents::close_agent::CloseAgentResult;
use crate::tools::handlers::multi_agents_spec::create_close_agent_tool_v2;
use crate::turn_timing::now_unix_timestamp_ms;
use codex_protocol::protocol::CollabCloseBeginEvent;
use codex_protocol::protocol::CollabCloseEndEvent;
use codex_tools::ToolSpec;

pub(crate) struct Handler;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("close_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_close_agent_tool_v2()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_close_agent(invocation).await.map(boxed_tool_output) })
    }
}

async fn handle_close_agent(
    invocation: ToolInvocation,
) -> Result<CloseAgentResult, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        call_id,
        ..
    } = invocation;
    let arguments = function_arguments(payload)?;
    let args: CloseAgentArgs = parse_arguments(&arguments)?;
    let agent_id = resolve_agent_target(&session, &turn, &args.target).await?;
    if agent_id == session.thread_id {
        return Err(FunctionCallError::RespondToModel(
            "an agent cannot close itself; return your result and let the parent close you"
                .to_string(),
        ));
    }
    let receiver_agent = session
        .services
        .agent_control
        .ensure_agent_known(agent_id)
        .map_err(|err| collab_agent_error(agent_id, err))?;
    let status = session.services.agent_control.get_status(agent_id).await;
    session
        .send_event(
            &turn,
            CollabCloseBeginEvent {
                call_id: call_id.clone(),
                started_at_ms: now_unix_timestamp_ms(),
                sender_thread_id: session.thread_id,
                receiver_thread_id: agent_id,
            }
            .into(),
        )
        .await;
    let result = Box::pin(session.services.agent_control.close_agent(agent_id))
        .await
        .map_err(|err| collab_agent_error(agent_id, err))
        .map(|_| ());
    session
        .send_event(
            &turn,
            CollabCloseEndEvent {
                call_id,
                completed_at_ms: now_unix_timestamp_ms(),
                sender_thread_id: session.thread_id,
                receiver_thread_id: agent_id,
                receiver_agent_nickname: receiver_agent.agent_nickname,
                receiver_agent_role: receiver_agent.agent_role,
                status: status.clone(),
            }
            .into(),
        )
        .await;
    result?;

    Ok(CloseAgentResult {
        previous_status: status,
    })
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseAgentArgs {
    target: String,
}
