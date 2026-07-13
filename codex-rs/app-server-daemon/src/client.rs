use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::InitializeResponse;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::RequestId;
use codex_uds::UnixStream;
use futures::SinkExt;
use futures::StreamExt;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

pub(crate) const CONTROL_SOCKET_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const CLIENT_NAME: &str = "codex_app_server_daemon";
const INITIALIZE_REQUEST_ID: RequestId = RequestId::Integer(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeInfo {
    pub(crate) app_server_version: String,
}

pub(crate) async fn probe(socket_path: &Path) -> Result<ProbeInfo> {
    timeout(CONTROL_SOCKET_RESPONSE_TIMEOUT, probe_inner(socket_path))
        .await
        .with_context(|| {
            format!(
                "timed out probing app-server control socket {}",
                socket_path.display()
            )
        })?
}

async fn probe_inner(socket_path: &Path) -> Result<ProbeInfo> {
    let mut websocket = connect(socket_path).await?;

    let initialize_response = initialize(&mut websocket, /*experimental_api*/ false).await?;
    let initialized = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    send_message(&mut websocket, &initialized)
        .await
        .context("failed to send initialized notification")?;
    websocket.close(None).await.ok();

    Ok(ProbeInfo {
        app_server_version: parse_version_from_user_agent(&initialize_response.user_agent)?,
    })
}

pub(crate) async fn connect(socket_path: &Path) -> Result<WebSocketStream<UnixStream>> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
    let (websocket, _response) = client_async("ws://localhost/", stream)
        .await
        .with_context(|| format!("failed to upgrade {}", socket_path.display()))?;
    Ok(websocket)
}

pub(crate) async fn proxy_json_lines<R, W>(
    socket_path: &Path,
    input: R,
    mut output: W,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let websocket = connect(socket_path).await?;
    let (mut websocket_writer, mut websocket_reader) = websocket.split();
    let mut input_lines = BufReader::new(input).lines();
    let mut input_open = true;

    loop {
        tokio::select! {
            input_line = input_lines.next_line(), if input_open => {
                match input_line.context("failed to read app-server proxy input")? {
                    Some(input_line) => websocket_writer
                        .send(Message::Text(input_line.into()))
                        .await
                        .context("failed to send app-server proxy request")?,
                    None => {
                        input_open = false;
                        websocket_writer
                            .close()
                            .await
                            .context("failed to close app-server proxy websocket")?;
                    }
                }
            }
            websocket_frame = websocket_reader.next() => {
                let Some(websocket_frame) = websocket_frame else {
                    break;
                };
                match websocket_frame.context("failed to read app-server proxy response")? {
                    Message::Text(payload) => {
                        output
                            .write_all(payload.as_bytes())
                            .await
                            .context("failed to write app-server proxy response")?;
                        output
                            .write_all(b"\n")
                            .await
                            .context("failed to terminate app-server proxy response")?;
                        output
                            .flush()
                            .await
                            .context("failed to flush app-server proxy response")?;
                    }
                    Message::Ping(payload) if input_open => websocket_writer
                        .send(Message::Pong(payload))
                        .await
                        .context("failed to answer app-server proxy ping")?,
                    Message::Close(_) => break,
                    Message::Binary(_)
                    | Message::Ping(_)
                    | Message::Pong(_)
                    | Message::Frame(_) => {}
                }
            }
        }
    }

    output
        .flush()
        .await
        .context("failed to flush app-server proxy output")?;
    Ok(())
}

pub(crate) async fn initialize<S>(
    websocket: &mut WebSocketStream<S>,
    experimental_api: bool,
) -> Result<InitializeResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let initialize = JSONRPCMessage::Request(JSONRPCRequest {
        id: INITIALIZE_REQUEST_ID,
        method: "initialize".to_string(),
        params: Some(serde_json::to_value(InitializeParams {
            client_info: ClientInfo {
                name: CLIENT_NAME.to_string(),
                title: Some("Codex App Server Daemon".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: if experimental_api {
                Some(InitializeCapabilities {
                    experimental_api: true,
                    ..Default::default()
                })
            } else {
                None
            },
        })?),
        trace: None,
    });
    send_message(websocket, &initialize)
        .await
        .context("failed to send initialize request")?;

    let response = loop {
        let message = timeout(CONTROL_SOCKET_RESPONSE_TIMEOUT, read_message(websocket))
            .await
            .context("timed out waiting for initialize response")??;
        if let JSONRPCMessage::Response(response) = message
            && response.id == INITIALIZE_REQUEST_ID
        {
            break response;
        }
    };
    serde_json::from_value::<InitializeResponse>(response.result)
        .context("failed to parse initialize response")
}

pub(crate) async fn send_message<S>(
    websocket: &mut WebSocketStream<S>,
    message: &JSONRPCMessage,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    websocket
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;
    Ok(())
}

pub(crate) async fn read_message<S>(websocket: &mut WebSocketStream<S>) -> Result<JSONRPCMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let frame = websocket
            .next()
            .await
            .ok_or_else(|| anyhow!("app-server closed the control socket"))??;
        let Message::Text(payload) = frame else {
            continue;
        };
        return serde_json::from_str::<JSONRPCMessage>(&payload)
            .context("failed to parse app-server JSON-RPC message");
    }
}

fn parse_version_from_user_agent(user_agent: &str) -> Result<String> {
    let (_originator, rest) = user_agent
        .split_once('/')
        .ok_or_else(|| anyhow!("app-server user-agent omitted version separator"))?;
    let version = rest
        .split_whitespace()
        .next()
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("app-server user-agent omitted version"))?;
    Ok(version.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use codex_uds::UnixListener;
    use futures::SinkExt;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tokio::io::duplex;
    use tokio::time::Duration;
    use tokio::time::timeout;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    use super::parse_version_from_user_agent;
    use super::proxy_json_lines;

    #[tokio::test]
    async fn proxy_upgrades_control_socket_and_relays_json_lines() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let socket_path = temp_dir.path().join("app-server.sock");
        let mut listener = UnixListener::bind(&socket_path)
            .await
            .expect("bind control socket");
        let server_task = tokio::spawn(async move {
            let stream = listener.accept().await.expect("accept proxy");
            let mut websocket = accept_async(stream).await.expect("upgrade proxy");
            assert_eq!(
                websocket
                    .next()
                    .await
                    .expect("request frame")
                    .expect("request"),
                Message::Text("request".into())
            );
            websocket
                .send(Message::Text("response".into()))
                .await
                .expect("send response");
            websocket
                .send(Message::Text("turn-completed".into()))
                .await
                .expect("send notification");
            websocket.close(None).await.expect("close websocket");
        });

        let (mut input_writer, input_reader) = duplex(1024);
        let (output_writer, output_reader) = duplex(1024);
        let proxy_socket_path = socket_path.clone();
        let proxy_task = tokio::spawn(async move {
            proxy_json_lines(&proxy_socket_path, input_reader, output_writer).await
        });

        input_writer
            .write_all(b"request\n")
            .await
            .expect("write request");
        let mut output_reader = BufReader::new(output_reader);
        let mut response = String::new();
        timeout(
            Duration::from_secs(1),
            output_reader.read_line(&mut response),
        )
        .await
        .expect("response timeout")
        .expect("read response");
        assert_eq!(response, "response\n");

        let mut notification = String::new();
        timeout(
            Duration::from_secs(1),
            output_reader.read_line(&mut notification),
        )
        .await
        .expect("notification timeout")
        .expect("read notification");
        assert_eq!(notification, "turn-completed\n");

        server_task.await.expect("server task");
        proxy_task.await.expect("proxy task").expect("proxy result");
    }

    #[test]
    fn parses_version_from_codex_user_agent() {
        assert_eq!(
            parse_version_from_user_agent(
                "codex_app_server_daemon/1.2.3 (Linux 6.8.0; x86_64) codex_cli_rs/1.2.3",
            )
            .expect("version"),
            "1.2.3"
        );
    }

    #[test]
    fn rejects_user_agent_without_version() {
        assert!(parse_version_from_user_agent("codex_app_server_daemon").is_err());
    }
}
