use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_app_server_client::AppServerClient;
use codex_app_server_client::AppServerEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

#[derive(Debug)]
pub(crate) enum DashboardEvent {
    Notification(Box<ServerNotification>),
    Lagged(usize),
    Disconnected(String),
}

pub(crate) struct DashboardClient {
    request_handle: AppServerRequestHandle,
    request_id: Arc<AtomicU64>,
    event_rx: mpsc::Receiver<DashboardEvent>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl DashboardClient {
    pub(crate) async fn connect(websocket_url: String) -> Result<Self> {
        let remote = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
            websocket_url,
            client_name: "codex-agent-dashboard".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            experimental_api: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 512,
        })
        .await
        .context("failed to connect to app-server")?;

        let mut client = AppServerClient::Remote(remote);
        let request_handle = client.request_handle();
        let (event_tx, event_rx) = mpsc::channel(512);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        let _ = client.shutdown().await;
                        break;
                    }
                    maybe_event = client.next_event() => {
                        let Some(event) = maybe_event else {
                            let _ = event_tx
                                .send(DashboardEvent::Disconnected(
                                    "app-server event stream closed".to_string(),
                                ))
                                .await;
                            break;
                        };
                        match event {
                            AppServerEvent::ServerNotification(notification) => {
                                if event_tx
                                    .send(DashboardEvent::Notification(Box::new(notification)))
                                    .await
                                    .is_err()
                                {
                                    let _ = client.shutdown().await;
                                    break;
                                }
                            }
                            AppServerEvent::Lagged { skipped } => {
                                if event_tx.send(DashboardEvent::Lagged(skipped)).await.is_err() {
                                    let _ = client.shutdown().await;
                                    break;
                                }
                            }
                            AppServerEvent::Disconnected { message } => {
                                let _ = event_tx.send(DashboardEvent::Disconnected(message)).await;
                                let _ = client.shutdown().await;
                                break;
                            }
                            AppServerEvent::ServerRequest(request) => {
                                let _ = client
                                    .reject_server_request(
                                        request.id().clone(),
                                        JSONRPCErrorError {
                                            code: -32001,
                                            message: "codex-agent-dashboard is read-only".to_string(),
                                            data: None,
                                        },
                                    )
                                    .await;
                            }
                            AppServerEvent::LegacyNotification(_) => {}
                        }
                    }
                }
            }
        });

        Ok(Self {
            request_handle,
            request_id: Arc::new(AtomicU64::new(1)),
            event_rx,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    pub(crate) async fn next_event(&mut self) -> Option<DashboardEvent> {
        self.event_rx.recv().await
    }

    pub(crate) async fn list_loaded_thread_ids(&self) -> Result<Vec<String>> {
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let response: ThreadLoadedListResponse = self
                .request_handle
                .request_typed(ClientRequest::ThreadLoadedList {
                    request_id: self.next_request_id(),
                    params: ThreadLoadedListParams {
                        cursor: cursor.clone(),
                        limit: None,
                    },
                })
                .await
                .with_context(|| {
                    format!(
                        "thread/loaded/list failed at cursor {}",
                        cursor.as_deref().unwrap_or("<start>")
                    )
                })?;
            ids.extend(response.data);
            let Some(next_cursor) = response.next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }
        Ok(ids)
    }

    pub(crate) async fn read_thread(&self, thread_id: &str, include_turns: bool) -> Result<Thread> {
        let response: ThreadReadResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadRead {
                request_id: self.next_request_id(),
                params: ThreadReadParams {
                    thread_id: thread_id.to_string(),
                    include_turns,
                },
            })
            .await
            .with_context(|| format!("thread/read failed for {thread_id}"))?;
        Ok(response.thread)
    }

    pub(crate) async fn load_threads(&self, include_turns: bool) -> Result<Vec<Thread>> {
        let ids = self.list_loaded_thread_ids().await?;
        let mut threads = Vec::with_capacity(ids.len());
        for id in ids {
            threads.push(self.read_thread(&id, include_turns).await?);
        }
        Ok(threads)
    }

    pub(crate) async fn load_root_threads(&self) -> Result<Vec<Thread>> {
        let mut roots = self
            .load_threads(/*include_turns*/ false)
            .await?
            .into_iter()
            .filter(|thread| thread.parent_thread_id.is_none())
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        if roots.is_empty() {
            bail!("no loaded root thread found; pass --parent-thread-id");
        }
        Ok(roots)
    }

    pub(crate) async fn shutdown(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Integer(self.request_id.fetch_add(1, Ordering::Relaxed) as i64)
    }
}
