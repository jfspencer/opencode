//! WebSockets (RFC 6455) over an HTTP/1.1 upgrade, as the TS server's raw handlers use them (`request.upgrade` and
//! effect's `Socket`, through `runPtySocket` in packages/server/src/handlers/pty-socket.ts).
//!
//! A handler answers the upgrade with [`switching`] and hands the request's pending upgrade to [`Socket::open`]. One
//! writer drains the handler's outbox (text, binary, then a close that ends the connection) while the reader passes
//! each message to the handler, in order; either side closing ends both. A client's close is echoed with its code,
//! pings are answered. No extension (compression) or subprotocol is negotiated.

use std::future::Future;

use base64::Engine;
use bytes::Bytes;
use hyper::header::HeaderMap;
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

use super::{Body, Response};

/// What a handler sends: frames in order, a close last.
#[derive(Debug)]
pub enum Outbound {
    Text(String),
    Binary(Bytes),
    Close(u16, String),
}

/// A message the client sent.
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The largest message read; a larger one ends the connection (1009).
const MAX_MESSAGE: usize = 16 << 20;

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The handshake's `Sec-WebSocket-Accept`, or `None` when the request is no WebSocket upgrade.
pub fn accept(headers: &HeaderMap) -> Option<String> {
    let upgrade = header(headers, "upgrade")?;
    if !upgrade.split(',').any(|token| token.trim().eq_ignore_ascii_case("websocket")) {
        return None;
    }
    let key = header(headers, "sec-websocket-key")?.trim();
    if key.is_empty() {
        return None;
    }
    let digest = Sha1::new().chain_update(key.as_bytes()).chain_update(GUID.as_bytes()).finalize();
    Some(base64::engine::general_purpose::STANDARD.encode(digest))
}

/// The `101 Switching Protocols` answer.
pub fn switching(accept: String) -> Response {
    Response {
        status: 101,
        headers: vec![("upgrade", "websocket".into()), ("connection", "Upgrade".into()), ("sec-websocket-accept", accept)],
        body: Body::Empty,
    }
}

type Io = TokioIo<Upgraded>;

pub struct Socket {
    reader: ReadHalf<Io>,
    writer: WriteHalf<Io>,
}

/// What the reader passes on, in the order the client sent it.
enum Inbound {
    Message(Message),
    /// The client closed (its code, if it sent one).
    Closed(Option<u16>),
    /// The client broke the protocol: the close code to send.
    Failed(u16),
}

impl Socket {
    /// Waits for the upgrade to complete (hyper has sent the 101); `None` if the client went away.
    pub async fn open(upgrade: OnUpgrade) -> Option<Self> {
        let upgraded = upgrade.await.ok()?;
        let (reader, writer) = tokio::io::split(TokioIo::new(upgraded));
        Some(Self { reader, writer })
    }

    /// Runs the connection until either side closes: the outbox drains to the client (ending after a
    /// [`Outbound::Close`]) while each message the client sends goes to `on_message`, one at a time.
    pub async fn run<H, F>(self, mut outbox: mpsc::UnboundedReceiver<Outbound>, mut on_message: H)
    where
        H: FnMut(Message) -> F,
        F: Future<Output = ()>,
    {
        let Socket { reader, mut writer } = self;
        let (inbound_tx, mut inbound) = mpsc::unbounded_channel();
        let (pong_tx, mut pongs) = mpsc::unbounded_channel::<Vec<u8>>();
        let reading = tokio::spawn(read_loop(reader, inbound_tx, pong_tx));
        let handle = async {
            loop {
                match inbound.recv().await {
                    Some(Inbound::Message(message)) => on_message(message).await,
                    Some(Inbound::Closed(code)) => return Some(code.map(|code| close_payload(code, ""))),
                    Some(Inbound::Failed(code)) => return Some(Some(close_payload(code, ""))),
                    None => return None,
                }
            }
        };
        let drain = async {
            let mut open = true;
            loop {
                tokio::select! {
                    biased;
                    Some(payload) = pongs.recv() => {
                        if write_frame(&mut writer, 0xA, &payload).await.is_err() {
                            return;
                        }
                    }
                    item = outbox.recv(), if open => match item {
                        Some(Outbound::Text(text)) => {
                            if write_frame(&mut writer, 0x1, text.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Some(Outbound::Binary(bytes)) => {
                            if write_frame(&mut writer, 0x2, &bytes).await.is_err() {
                                return;
                            }
                        }
                        Some(Outbound::Close(code, reason)) => {
                            let _ = write_frame(&mut writer, 0x8, &close_payload(code, &reason)).await;
                            return;
                        }
                        // Nothing more will be sent; the connection stays until the client closes it.
                        None => open = false,
                    },
                }
            }
        };
        let ended = tokio::select! {
            ended = handle => Some(ended),
            _ = drain => None,
        };
        match ended {
            // The client closed (or broke the protocol): its close is answered.
            Some(Some(payload)) => {
                let _ = write_frame(&mut writer, 0x8, &payload.unwrap_or_default()).await;
            }
            Some(None) => {}
            // The server closed: the client answers with its own close, read before the connection goes, or the
            // unread answer would reset the connection under the close frame sent.
            None => {
                let answer = async {
                    while let Some(item) = inbound.recv().await {
                        if !matches!(item, Inbound::Message(_)) {
                            break;
                        }
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), answer).await;
            }
        }
        let _ = writer.shutdown().await;
        reading.abort();
    }
}

/// Reads the client's frames until it closes or the connection ends.
async fn read_loop(mut reader: ReadHalf<Io>, inbound: mpsc::UnboundedSender<Inbound>, pongs: mpsc::UnboundedSender<Vec<u8>>) {
    let mut partial: Option<(u8, Vec<u8>)> = None;
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(Some(code)) => {
                let _ = inbound.send(Inbound::Failed(code));
                return;
            }
            Err(None) => return,
        };
        let item = match frame.opcode {
            0x0 | 0x1 | 0x2 => {
                let (opcode, mut data) = match (frame.opcode, partial.take()) {
                    (0x0, Some(started)) => started,
                    (0x0, None) | (_, Some(_)) => {
                        let _ = inbound.send(Inbound::Failed(1002));
                        return;
                    }
                    (opcode, None) => (opcode, Vec::new()),
                };
                if data.len() + frame.payload.len() > MAX_MESSAGE {
                    let _ = inbound.send(Inbound::Failed(1009));
                    return;
                }
                data.extend_from_slice(&frame.payload);
                if !frame.fin {
                    partial = Some((opcode, data));
                    continue;
                }
                if opcode == 0x2 {
                    Inbound::Message(Message::Binary(data))
                } else {
                    match String::from_utf8(data) {
                        Ok(text) => Inbound::Message(Message::Text(text)),
                        Err(_) => Inbound::Failed(1007),
                    }
                }
            }
            0x8 => Inbound::Closed((frame.payload.len() >= 2).then(|| u16::from_be_bytes([frame.payload[0], frame.payload[1]]))),
            0x9 => {
                let _ = pongs.send(frame.payload);
                continue;
            }
            0xA => continue,
            _ => Inbound::Failed(1002),
        };
        let last = !matches!(item, Inbound::Message(_));
        if inbound.send(item).is_err() || last {
            return;
        }
    }
}

fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    // A control frame's payload is at most 125 bytes.
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    payload
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// One frame from the client, unmasked; `Err` with the close code to send (none when the connection broke).
async fn read_frame(reader: &mut ReadHalf<Io>) -> Result<Frame, Option<u16>> {
    let mut head = [0u8; 2];
    reader.read_exact(&mut head).await.map_err(|_| None)?;
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let length = match head[1] & 0x7f {
        126 => {
            let mut bytes = [0u8; 2];
            reader.read_exact(&mut bytes).await.map_err(|_| None)?;
            u16::from_be_bytes(bytes) as u64
        }
        127 => {
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes).await.map_err(|_| None)?;
            u64::from_be_bytes(bytes)
        }
        n => n as u64,
    };
    // Clients mask every frame (RFC 6455 §5.1).
    if !masked {
        return Err(Some(1002));
    }
    if length > MAX_MESSAGE as u64 {
        return Err(Some(1009));
    }
    let mut mask = [0u8; 4];
    reader.read_exact(&mut mask).await.map_err(|_| None)?;
    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await.map_err(|_| None)?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Ok(Frame { fin, opcode, payload })
}

/// Writes one frame in one write: Bun's client loses a close frame's code when its payload arrives in a later read.
async fn write_frame(writer: &mut WriteHalf<Io>, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(10 + payload.len());
    head.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n <= u16::MAX as usize => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    head.extend_from_slice(payload);
    writer.write_all(&head).await?;
    writer.flush().await
}
