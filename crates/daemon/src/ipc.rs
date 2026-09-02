// The private Unix-socket IPC server: listener ownership, per-connection
// framing, and the bounded command queue each connection runs.

use crate::commands::handle_command;
use crate::jobs;
use crate::presence::reconcile_connection_intent;
use crate::state::Shared;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use omarchy_whatsapp_protocol::{ClientFrame, PROTOCOL_VERSION, ServerEvent, ServerFrame};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::debug;

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn bind_private_listener(socket: &Path) -> Result<UnixListener> {
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                bail!(
                    "refusing to replace non-socket IPC path {}",
                    socket.display()
                );
            }
            match UnixStream::connect(socket).await {
                Ok(_) => bail!("another omarchy-whatsapp daemon is already running"),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    std::fs::remove_file(socket)
                        .with_context(|| format!("removing stale socket {}", socket.display()))?;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("checking existing socket {}", socket.display()));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting socket {}", socket.display()));
        }
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn serve(listener: UnixListener, shared: Arc<Shared>) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let connection_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, connection_shared).await {
                tracing::debug!(%error, "IPC client disconnected");
            }
        });
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn write_connection_sync(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    shared: &Shared,
) -> Result<()> {
    write_frame(
        write,
        &shared.response(
            None,
            ServerEvent::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        ),
    )
    .await?;
    write_frame(write, &shared.response(None, shared.state_event().await)).await?;
    write_frame(
        write,
        &shared.response(None, shared.chat_state_resync_event().await),
    )
    .await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn serve_connection(stream: UnixStream, shared: Arc<Shared>) -> Result<()> {
    let connection_id = shared.open_connection();
    let result = serve_connection_inner(stream, Arc::clone(&shared), connection_id).await;
    let (before, after) = shared.close_connection(connection_id);
    reconcile_connection_intent(&shared, &before, &after).await;
    result
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn serve_connection_inner(
    stream: UnixStream,
    shared: Arc<Shared>,
    connection_id: u64,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    // Bound memory even if another process owned by the same user sends a line
    // without a delimiter. Normal commands are only a few kilobytes.
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(128 * 1024));
    let mut events = shared.events.subscribe();
    let (responses, mut response_queue) = mpsc::channel::<ServerFrame>(32);
    let permits = Arc::new(Semaphore::new(jobs::MAX_CONNECTION_JOBS));
    let queue_slots = Arc::new(Semaphore::new(jobs::MAX_QUEUED_CONNECTION_JOBS));
    let mut command_jobs = JoinSet::new();
    write_connection_sync(&mut write, &shared).await?;

    loop {
        tokio::select! {
            line = lines.next() => {
                let Some(line) = line else { return Ok(()); };
                let line = line.context("reading IPC request")?;
                let frame = match serde_json::from_str::<ClientFrame>(&line) {
                    Ok(frame) => frame,
                    Err(error) => {
                        write_frame(&mut write, &shared.response(None, ServerEvent::Error {
                            message: format!("invalid request: {error}"),
                        })).await?;
                        continue;
                    }
                };
                let id = frame.id;
                let Ok(queue_slot) = Arc::clone(&queue_slots).try_acquire_owned() else {
                    write_frame(&mut write, &shared.response(id, ServerEvent::Error {
                        message: "too many active requests".to_owned(),
                    })).await?;
                    continue;
                };
                let timeout = jobs::timeout(&frame.command);
                let conflict_key = jobs::conflict_key(&frame.command);
                let command_shared = Arc::clone(&shared);
                let command_responses = responses.clone();
                let command_permits = Arc::clone(&permits);
                command_jobs.spawn(async move {
                    let _queue_slot = queue_slot;
                    // The deadline covers waiting for an execution permit and
                    // any conflict gate, so a queued command is bounded end to
                    // end instead of stalling indefinitely behind stuck work.
                    let event = match tokio::time::timeout(timeout, async {
                        let _permit = command_permits.acquire_owned().await.ok();
                        let gate = if let Some(key) = conflict_key {
                            Some(command_shared.command_gate(&key).await)
                        } else {
                            None
                        };
                        let _gate = if let Some(gate) = &gate {
                            Some(gate.lock().await)
                        } else {
                            None
                        };
                        handle_command(frame.command, &command_shared, connection_id).await
                    })
                    .await
                    {
                        Ok(Ok(event)) => event,
                        Ok(Err(error)) => ServerEvent::Error {
                            message: error.to_string(),
                        },
                        Err(_) => ServerEvent::Error {
                            message: "request timed out".to_owned(),
                        },
                    };
                    let response = command_shared.response(id, event);
                    let _ = command_responses.send(response).await;
                });
            },
            event = events.recv() => match event {
                Ok(event) => write_frame(&mut write, &event).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Repeating the versioned hello makes lag recovery explicit:
                    // compatible clients refresh every authoritative snapshot,
                    // rather than treating a state frame as a complete resync.
                    write_connection_sync(&mut write, &shared).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            response = response_queue.recv() => {
                let Some(response) = response else { return Ok(()); };
                write_frame(&mut write, &response).await?;
            },
            completed = command_jobs.join_next(), if !command_jobs.is_empty() => {
                if let Some(Err(error)) = completed {
                    debug!(%error, "IPC command job stopped unexpectedly");
                }
            },
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn write_frame(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &ServerFrame,
) -> Result<()> {
    let mut json = serde_json::to_vec(frame)?;
    if json.len() > jobs::MAX_IPC_FRAME_BYTES {
        bail!("IPC frame exceeds the 16 MiB byte budget");
    }
    json.push(b'\n');
    write.write_all(&json).await?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::assets;
    use crate::test_support::{read_test_frame, test_shared};
    use omarchy_whatsapp_protocol::{ChatStateResyncStatus, Command};
    use std::collections::HashMap;

    #[tokio::test]
    async fn listener_rejects_live_daemons_and_non_socket_paths_but_replaces_stale_sockets() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        std::fs::write(&socket, b"do not replace").unwrap();
        assert!(bind_private_listener(&socket).await.is_err());
        assert_eq!(std::fs::read(&socket).unwrap(), b"do not replace");
        std::fs::remove_file(&socket).unwrap();

        let live = UnixListener::bind(&socket).unwrap();
        let error = bind_private_listener(&socket).await.unwrap_err();
        assert!(error.to_string().contains("already running"));
        drop(live);

        let rebound = bind_private_listener(&socket).await.unwrap();
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(rebound);
    }

    #[tokio::test]
    async fn ipc_broadcasts_continue_while_a_command_is_waiting() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let client_guard = shared.client.write().await;
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..3 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        let request =
            serde_json::to_vec(&ClientFrame::new(Some(7), Command::ListChats { limit: 10 }))
                .unwrap();
        client_stream.write_all(&request).await.unwrap();
        client_stream.write_all(b"\n").await.unwrap();
        tokio::task::yield_now().await;
        shared
            .events
            .send(ServerFrame::event(ServerEvent::Unread { total: 9 }))
            .unwrap();

        buffer.clear();
        let broadcast = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(broadcast.event, ServerEvent::Unread { total: 9 });

        drop(client_guard);
        buffer.clear();
        let response = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(response.id, Some(7));
        assert!(matches!(response.event, ServerEvent::Chats { .. }));

        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn lagged_ipc_clients_receive_a_versioned_resync_handshake() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..3 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        let oversized = "x".repeat(96 * 1024);
        for _ in 0..24 {
            shared
                .events
                .send(ServerFrame::event(ServerEvent::Error {
                    message: oversized.clone(),
                }))
                .unwrap();
        }

        let hello = loop {
            buffer.clear();
            let frame = read_test_frame(&mut client_stream, &mut buffer).await;
            if matches!(frame.event, ServerEvent::Hello { .. }) {
                break frame;
            }
        };
        assert_eq!(
            hello.event,
            ServerEvent::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
        buffer.clear();
        assert!(matches!(
            read_test_frame(&mut client_stream, &mut buffer).await.event,
            ServerEvent::State { .. }
        ));
        buffer.clear();
        assert_eq!(
            read_test_frame(&mut client_stream, &mut buffer).await.event,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Idle,
                message: None,
            }
        );

        drop(client_stream);
        // Pending oversized broadcasts can observe the intentional client
        // close as a broken pipe; the connection task itself must not panic.
        let _ = server.await.unwrap();
    }

    #[tokio::test]
    async fn ipc_command_queue_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let client_guard = shared.client.write().await;
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..3 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        // Every command past the queue bound is rejected while the held client
        // lock keeps the queued ones from draining.
        let overflow = 6;
        let total = jobs::MAX_QUEUED_CONNECTION_JOBS + overflow;
        for id in 0..total {
            let request = serde_json::to_vec(&ClientFrame::new(
                Some(id as u64),
                Command::ListChats { limit: 10 },
            ))
            .unwrap();
            client_stream.write_all(&request).await.unwrap();
            client_stream.write_all(b"\n").await.unwrap();
        }

        buffer.clear();
        let response = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(
            response.event,
            ServerEvent::Error {
                message: "too many active requests".into(),
            }
        );

        drop(client_guard);
        for _ in 1..total {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }
        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn ipc_commands_queue_beyond_the_parallel_job_limit() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..3 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        // A burst larger than the parallel limit is normal for a busy chat; it
        // must queue rather than be rejected.
        let burst = jobs::MAX_CONNECTION_JOBS * 4;
        for id in 0..burst {
            let request =
                serde_json::to_vec(&ClientFrame::new(Some(id as u64), Command::Ping)).unwrap();
            client_stream.write_all(&request).await.unwrap();
            client_stream.write_all(b"\n").await.unwrap();
        }

        let mut acknowledged = 0;
        while acknowledged < burst {
            buffer.clear();
            let response = read_test_frame(&mut client_stream, &mut buffer).await;
            if response.id.is_none() {
                continue;
            }
            assert_eq!(response.event, ServerEvent::Pong);
            acknowledged += 1;
        }

        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn outbox_listings_are_answered_while_a_delivery_holds_its_gate() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.voice_outbox_dir).unwrap();
        // An upload and a queued text send own these gates for as long as the
        // network takes; opening the panel still lists both outboxes.
        let voice_guard = shared.voice_outbox_gate.lock().await;
        let text_guard = shared.text_outbox_gate.lock().await;
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..3 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        for (id, command) in [
            (11_u64, Command::ListVoiceOutbox),
            (12, Command::ListTextOutbox),
        ] {
            let request = serde_json::to_vec(&ClientFrame::new(Some(id), command)).unwrap();
            client_stream.write_all(&request).await.unwrap();
            client_stream.write_all(b"\n").await.unwrap();
        }

        let mut answered = HashMap::new();
        while answered.len() < 2 {
            buffer.clear();
            let frame = read_test_frame(&mut client_stream, &mut buffer).await;
            if let Some(id) = frame.id {
                answered.insert(id, frame.event);
            }
        }
        assert_eq!(
            answered.remove(&11),
            Some(ServerEvent::VoiceOutbox {
                entries: Vec::new(),
            })
        );
        assert_eq!(
            answered.remove(&12),
            Some(ServerEvent::TextOutbox {
                entries: Vec::new(),
            })
        );

        drop((voice_guard, text_guard));
        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }
}
