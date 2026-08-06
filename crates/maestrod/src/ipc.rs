use std::{collections::HashMap, io, path::Path};

use maestro_domain::{MaestroError, RequestId};
use maestro_protocol::{
    AuthenticationToken, ClientFrame, ClientHello, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request,
    RequestEnvelope, Response, ServerEvent, ServerFrame, decode_payload, encode_frame,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    sync::{broadcast, mpsc, oneshot},
};

use crate::DaemonPaths;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("IPC frame is invalid: {0}")]
    Codec(String),
    #[error("IPC frame prefix is truncated: received {actual} of 4 bytes")]
    TruncatedPrefix { actual: usize },
    #[error("daemon rejected the connection: {0:?}")]
    Fatal(MaestroError),
    #[error("daemon returned an unexpected frame")]
    UnexpectedFrame,
    #[error("daemon IPC connection closed")]
    Disconnected,
}

#[derive(Debug)]
struct OutboundRequest {
    request_id: RequestId,
    request: Request,
    completion: oneshot::Sender<Result<Response, IpcError>>,
}

/// Cloneable correlated client for one persistent authenticated daemon socket.
///
/// Requests may be issued concurrently and responses may arrive out of order.
/// Server events are exposed through a bounded broadcast channel so a slow
/// desktop view cannot block daemon persistence or unrelated requests.
#[derive(Debug, Clone)]
pub struct MultiplexedDaemonClient {
    outbound: mpsc::Sender<OutboundRequest>,
    events: broadcast::Sender<ServerEvent>,
}

impl MultiplexedDaemonClient {
    /// Connects and starts the bounded request/event driver.
    ///
    /// # Errors
    ///
    /// Returns an error when token loading, socket I/O, authentication, or
    /// protocol negotiation fails.
    pub async fn connect(
        paths: &DaemonPaths,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Result<Self, IpcError> {
        let token = paths
            .load_token()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut stream = UnixStream::connect(&paths.socket).await?;
        write_frame(
            &mut stream,
            &ClientFrame::Hello(ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_name: client_name.into(),
                client_version: client_version.into(),
                authentication_token: AuthenticationToken::new(token.expose()),
            }),
        )
        .await?;
        match read_frame::<ServerFrame, _>(&mut stream).await? {
            Some(ServerFrame::Hello(_)) => {}
            Some(ServerFrame::Fatal(error)) => return Err(IpcError::Fatal(error)),
            _ => return Err(IpcError::UnexpectedFrame),
        }

        let (outbound, requests) = mpsc::channel(128);
        let (events, _) = broadcast::channel(1_024);
        tokio::spawn(run_multiplexed_driver(stream, requests, events.clone()));
        Ok(Self { outbound, events })
    }

    /// Sends one request through the persistent socket.
    ///
    /// # Errors
    ///
    /// Returns a daemon error, transport error, or disconnect indication.
    pub async fn request(&self, request: Request) -> Result<Response, IpcError> {
        self.request_correlated(RequestId::new(), request).await
    }

    /// Sends one request with a caller-provided protocol correlation ID.
    ///
    /// The caller must keep IDs unique among requests that are simultaneously
    /// in flight on this connection.
    ///
    /// # Errors
    ///
    /// Returns a daemon error, transport error, or disconnect indication.
    pub async fn request_correlated(
        &self,
        request_id: RequestId,
        request: Request,
    ) -> Result<Response, IpcError> {
        let (completion, response) = oneshot::channel();
        self.outbound
            .send(OutboundRequest {
                request_id,
                request,
                completion,
            })
            .await
            .map_err(|_| IpcError::Disconnected)?;
        response.await.map_err(|_| IpcError::Disconnected)?
    }

    /// Returns whether both handles use the same authenticated daemon connection.
    #[must_use]
    pub fn is_same_connection(&self, other: &Self) -> bool {
        self.outbound.same_channel(&other.outbound)
    }

    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }
}

async fn run_multiplexed_driver(
    stream: UnixStream,
    mut outbound: mpsc::Receiver<OutboundRequest>,
    events: broadcast::Sender<ServerEvent>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let mut pending = HashMap::<RequestId, oneshot::Sender<Result<Response, IpcError>>>::new();
    loop {
        tokio::select! {
            outbound_request = outbound.recv() => {
                let Some(outbound_request) = outbound_request else {
                    break;
                };
                let frame = ClientFrame::Request(RequestEnvelope {
                    request_id: outbound_request.request_id,
                    request: outbound_request.request,
                });
                pending.insert(outbound_request.request_id, outbound_request.completion);
                if write_frame(&mut writer, &frame).await.is_err() {
                    break;
                }
            }
            frame = read_frame::<ServerFrame, _>(&mut reader) => {
                match frame {
                    Ok(Some(ServerFrame::Response(envelope))) => {
                        if let Some(completion) = pending.remove(&envelope.request_id) {
                            let _ = completion.send(envelope.response.map_err(IpcError::Fatal));
                        }
                    }
                    Ok(Some(ServerFrame::Event(event))) => {
                        let _ = events.send(event);
                    }
                    Ok(Some(ServerFrame::Fatal(error))) => {
                        for (_, completion) in pending.drain() {
                            let _ = completion.send(Err(IpcError::Fatal(error.clone())));
                        }
                        break;
                    }
                    Ok(Some(ServerFrame::Hello(_)) | None) | Err(_) => break,
                }
            }
        }
    }
    for (_, completion) in pending {
        let _ = completion.send(Err(IpcError::Disconnected));
    }
}

pub(crate) async fn read_frame<T, R>(reader: &mut R) -> Result<Option<T>, IpcError>
where
    T: serde::de::DeserializeOwned,
    R: AsyncRead + Unpin,
{
    read_frame_with_limit(reader, MAX_FRAME_BYTES).await
}

pub(crate) async fn read_frame_with_limit<T, R>(
    reader: &mut R,
    maximum_bytes: usize,
) -> Result<Option<T>, IpcError>
where
    T: serde::de::DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let mut prefix = [0_u8; 4];
    let mut prefix_bytes = reader.read(&mut prefix).await?;
    if prefix_bytes == 0 {
        return Ok(None);
    }
    while prefix_bytes < prefix.len() {
        let read = reader.read(&mut prefix[prefix_bytes..]).await?;
        if read == 0 {
            return Err(IpcError::TruncatedPrefix {
                actual: prefix_bytes,
            });
        }
        prefix_bytes += read;
    }
    let length = u32::from_be_bytes(prefix) as usize;
    let maximum_bytes = maximum_bytes.min(MAX_FRAME_BYTES);
    if length > maximum_bytes {
        return Err(IpcError::Codec(format!(
            "frame length {length} exceeds maximum {maximum_bytes}"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    decode_payload(&payload)
        .map(Some)
        .map_err(|error| IpcError::Codec(error.to_string()))
}

pub(crate) async fn write_frame<T, W>(writer: &mut W, frame: &T) -> Result<(), IpcError>
where
    T: serde::Serialize,
    W: AsyncWrite + Unpin,
{
    let encoded = encode_frame(frame).map_err(|error| IpcError::Codec(error.to_string()))?;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Debug)]
pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    /// Connects with the token from the daemon's private runtime directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the token cannot be read, the socket cannot be
    /// reached, or the daemon rejects the handshake.
    pub async fn connect(
        paths: &DaemonPaths,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Result<Self, IpcError> {
        let token = paths
            .load_token()
            .map_err(|error| io::Error::other(error.to_string()))?;
        Self::connect_with(
            &paths.socket,
            ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_name: client_name.into(),
                client_version: client_version.into(),
                authentication_token: AuthenticationToken::new(token.expose()),
            },
        )
        .await
    }

    /// Connects using an explicit hello, primarily for compatibility tooling.
    ///
    /// # Errors
    ///
    /// Returns an error when I/O, framing, authentication, or negotiation fails.
    pub async fn connect_with(path: &Path, hello: ClientHello) -> Result<Self, IpcError> {
        let mut stream = UnixStream::connect(path).await?;
        write_frame(&mut stream, &ClientFrame::Hello(hello)).await?;
        match read_frame::<ServerFrame, _>(&mut stream).await? {
            Some(ServerFrame::Hello(_)) => Ok(Self { stream }),
            Some(ServerFrame::Fatal(error)) => Err(IpcError::Fatal(error)),
            _ => Err(IpcError::UnexpectedFrame),
        }
    }

    /// Sends one correlated request and waits for its response.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failures, daemon errors, or a response
    /// carrying a different correlation identifier.
    pub async fn request(&mut self, request: Request) -> Result<Response, IpcError> {
        let request_id = RequestId::new();
        write_frame(
            &mut self.stream,
            &ClientFrame::Request(RequestEnvelope {
                request_id,
                request,
            }),
        )
        .await?;
        match read_frame::<ServerFrame, _>(&mut self.stream).await? {
            Some(ServerFrame::Response(envelope)) if envelope.request_id == request_id => {
                envelope.response.map_err(IpcError::Fatal)
            }
            Some(ServerFrame::Fatal(error)) => Err(IpcError::Fatal(error)),
            _ => Err(IpcError::UnexpectedFrame),
        }
    }
}

#[cfg(test)]
mod tests {
    use maestro_protocol::{ClientFrame, MAX_HELLO_FRAME_BYTES};
    use tokio::io::{AsyncWriteExt, duplex};

    use super::{IpcError, read_frame, read_frame_with_limit};

    #[tokio::test]
    async fn clean_eof_is_distinct_from_every_truncated_prefix_length() {
        let (writer, mut reader) = duplex(16);
        drop(writer);
        assert!(
            read_frame::<ClientFrame, _>(&mut reader)
                .await
                .expect("clean EOF")
                .is_none()
        );

        for actual in 1..=3 {
            let (mut writer, mut reader) = duplex(16);
            writer
                .write_all(&[0_u8; 3][..actual])
                .await
                .expect("prefix writes");
            drop(writer);

            assert!(matches!(
                read_frame::<ClientFrame, _>(&mut reader).await,
                Err(IpcError::TruncatedPrefix { actual: received }) if received == actual
            ));
        }
    }

    #[tokio::test]
    async fn pre_auth_frame_limit_is_enforced_before_payload_allocation() {
        let (mut writer, mut reader) = duplex(16);
        let oversized = u32::try_from(MAX_HELLO_FRAME_BYTES + 1).expect("length fits");
        writer
            .write_all(&oversized.to_be_bytes())
            .await
            .expect("prefix writes");

        assert!(matches!(
            read_frame_with_limit::<ClientFrame, _>(&mut reader, MAX_HELLO_FRAME_BYTES).await,
            Err(IpcError::Codec(message)) if message.contains("exceeds maximum")
        ));
    }
}
