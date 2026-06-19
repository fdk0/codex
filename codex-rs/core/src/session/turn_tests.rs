use super::*;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnItemContributor;
use codex_protocol::AgentPath;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::protocol::InterAgentCommunication;
use pretty_assertions::assert_eq;
use std::sync::Arc;

struct RewriteAgentMessageContributor;

impl TurnItemContributor for RewriteAgentMessageContributor {
    fn contribute<'a>(
        &'a self,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> codex_extension_api::ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let TurnItem::AgentMessage(agent_message) = item {
                agent_message.content = vec![AgentMessageContent::Text {
                    text: "plan contributed assistant text".to_string(),
                }];
            }
            Ok(())
        })
    }
}

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        metadata: None,
    }
}

#[tokio::test]
async fn plan_mode_uses_contributed_turn_item_for_last_agent_message() {
    let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::new();
    builder.turn_item_contributor(Arc::new(RewriteAgentMessageContributor));
    session.services.extensions = Arc::new(builder.build());
    let turn_store = ExtensionData::new(turn_context.sub_id.clone());
    let mut state = PlanModeStreamState::new(&turn_context.sub_id);
    let mut last_agent_message = None;
    let item = assistant_output_text("original assistant text");

    let handled = handle_assistant_item_done_in_plan_mode(
        &session,
        &turn_context,
        &turn_store,
        &item,
        &mut state,
        /*previously_active_item*/ None,
        &mut last_agent_message,
    )
    .await;

    assert!(handled);
    assert_eq!(
        last_agent_message.as_deref(),
        Some("plan contributed assistant text")
    );
}

#[test]
fn split_leading_non_user_input_preserves_user_input_boundary() {
    let response_item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "synthetic wake".to_string(),
        }],
        phase: None,
        metadata: None,
    };
    let first_mail = InterAgentCommunication::new(
        AgentPath::root().join("worker").expect("worker path"),
        AgentPath::root(),
        Vec::new(),
        "first".to_string(),
        /*trigger_turn*/ true,
    );
    let second_mail = InterAgentCommunication::new(
        AgentPath::root().join("reviewer").expect("reviewer path"),
        AgentPath::root(),
        Vec::new(),
        "second".to_string(),
        /*trigger_turn*/ false,
    );
    let user_input = TurnInput::UserInput {
        content: vec![UserInput::Text {
            text: "operator steer".to_string(),
            text_elements: Vec::new(),
        }],
        client_id: None,
    };

    let (early_input, deferred_input) = split_leading_non_user_input(vec![
        TurnInput::ResponseItem(response_item.clone()),
        TurnInput::InterAgentCommunication(first_mail.clone()),
        user_input.clone(),
        TurnInput::InterAgentCommunication(second_mail.clone()),
    ]);

    assert_eq!(
        early_input,
        vec![
            TurnInput::ResponseItem(response_item),
            TurnInput::InterAgentCommunication(first_mail),
        ]
    );
    assert_eq!(
        deferred_input,
        vec![user_input, TurnInput::InterAgentCommunication(second_mail),]
    );
}
