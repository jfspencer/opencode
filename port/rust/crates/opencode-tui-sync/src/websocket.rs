//! A WebSocket client (RFC 6455) for the terminal panes, the runtime's `WebSocket` as `createPersistentPtyClient`
//! uses it: the HTTP/1.1 upgrade over [`crate::transport::open_stream`] (TLS and the proxy as `fetch` has them),
//! text and binary messages in both directions, and a close.
//!
//! The socket is two tasks: a reader that reports each message the server sends and a writer that drains what the
//! pane sends (client frames are masked, RFC 6455 §5.1). Ping frames are answered, a server close ends both. No
//! extension (compression) or subprotocol is negotiated.

use std::cell::Cell;
use std::rc::Rc;

use base64::Engine;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

use crate::abort::Reason;
use crate::transport;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The largest message read; a larger one ends the connection (1009).
const MAX_MESSAGE: usize = 16 << 20;

/// A message the server sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Text(String),
    Binary(Vec<u8>),
    /// The server closed (its code, if it sent one) or the connection ended.
    Closed(Option<u16>),
}

/// What the pane sends.
#[derive(Debug)]
enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Pong(Vec<u8>),
    Close(u16, String),
}

/// `socket.send(..)`, and the close the pane's cleanup sends: the writer half alone.
#[derive(Clone)]
pub struct SocketHandle {
    outgoing: mpsc::UnboundedSender<Outgoing>,
    open: Rc<Cell<bool>>,
}

impl SocketHandle {
    /// `socket.send(data)` with a binary message.
    pub fn send_binary(&self, data: Vec<u8>) {
        if self.open.get() {
            let _ = self.outgoing.send(Outgoing::Binary(data));
        }
    }

    /// `socket.send(text)`.
    pub fn send_text(&self, text: String) {
        if self.open.get() {
            let _ = self.outgoing.send(Outgoing::Text(text));
        }
    }

    /// `socket.close(code)`.
    pub fn close(&self) {
        if self.open.replace(false) {
            let _ = self.outgoing.send(Outgoing::Close(1000, String::new()));
        }
    }

    /// `socket.readyState === WebSocket.OPEN`.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }
}

/// An open WebSocket: the reader half, with [`Socket::handle`] for sending and closing.
pub struct Socket {
    handle: SocketHandle,
    incoming: mpsc::UnboundedReceiver<Incoming>,
}

impl Socket {
    pub fn handle(&self) -> SocketHandle {
        self.handle.clone()
    }

    /// The next message; `None` once the socket has ended.
    pub async fn recv(&mut self) -> Option<Incoming> {
        self.incoming.recv().await
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.handle.close();
    }
}

/// Opens a `ws(s)://` URL with the given handshake headers (`authorization`, ...).
pub async fn open(url: &str, headers: &[(String, String)]) -> Result<Socket, Reason> {
    let http = if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        url.to_owned()
    };
    let target = transport::open_stream(&http).await.map_err(|failure| Reason {
        name: "TypeError".into(),
        message: transport::fetch_error(&failure).message,
    })?;
    let mut key = [0u8; 16];
    crate::runtime::random(&mut key);
    let key = base64::engine::general_purpose::STANDARD.encode(key);
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(target.stream)).await.map_err(|error| Reason {
            name: "TypeError".into(),
            message: error.to_string(),
        })?;
    // The upgrade arrives on the connection future, which must be polled for it.
    tokio::task::spawn_local(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut request = hyper::Request::builder()
        .method("GET")
        .uri(target.path)
        .header("Host", target.host)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", key.clone())
        .header("Sec-WebSocket-Version", "13");
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let request = request
        .body(Full::new(Bytes::new()))
        .map_err(|error| Reason { name: "TypeError".into(), message: error.to_string() })?;
    let mut response = sender.send_request(request).await.map_err(|error| Reason {
        name: "TypeError".into(),
        message: error.to_string(),
    })?;
    if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Reason::error(format!("WebSocket handshake failed: {}", response.status())));
    }
    let expected = base64::engine::general_purpose::STANDARD
        .encode(Sha1::new().chain_update(key.as_bytes()).chain_update(GUID.as_bytes()).finalize());
    let accept = response
        .headers()
        .get("sec-websocket-accept")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if accept != expected {
        return Err(Reason::error("WebSocket handshake failed: bad Sec-WebSocket-Accept"));
    }
    let upgraded = hyper::upgrade::on(&mut response).await.map_err(|error| Reason {
        name: "TypeError".into(),
        message: error.to_string(),
    })?;
    let (reader, writer) = tokio::io::split(TokioIo::new(upgraded));
    let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
    let (incoming_tx, incoming) = mpsc::unbounded_channel();
    let open = Rc::new(Cell::new(true));
    tokio::task::spawn_local(write_loop(writer, outgoing_rx, open.clone()));
    tokio::task::spawn_local(read_loop(reader, incoming_tx, outgoing.clone(), open.clone()));
    Ok(Socket { handle: SocketHandle { outgoing, open }, incoming })
}

type Io = TokioIo<hyper::upgrade::Upgraded>;

async fn write_loop(mut writer: WriteHalf<Io>, mut outgoing: mpsc::UnboundedReceiver<Outgoing>, open: Rc<Cell<bool>>) {
    let mut mask = [0u8; 4];
    crate::runtime::random(&mut mask);
    let mut state = u32::from_le_bytes(mask).wrapping_mul(0x9E37_79B9);
    while let Some(item) = outgoing.recv().await {
        let (opcode, payload) = match item {
            Outgoing::Text(text) => (0x1, text.into_bytes()),
            Outgoing::Binary(data) => (0x2, data),
            Outgoing::Pong(data) => (0xA, data),
            Outgoing::Close(code, reason) => (0x8, close_payload(code, &reason)),
        };
        if write_frame(&mut writer, opcode, &payload, &mut state).await.is_err() {
            break;
        }
        if opcode == 0x8 {
            break;
        }
    }
    open.set(false);
    let _ = writer.shutdown().await;
}

async fn read_loop(
    mut reader: ReadHalf<Io>,
    incoming: mpsc::UnboundedSender<Incoming>,
    outgoing: mpsc::UnboundedSender<Outgoing>,
    open: Rc<Cell<bool>>,
) {
    let mut partial: Option<(u8, Vec<u8>)> = None;
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(_) => break,
        };
        match frame.opcode {
            0x0..=0x2 => {
                let (opcode, mut data) = match (frame.opcode, partial.take()) {
                    (0x0, Some(started)) => started,
                    (0x0, None) | (_, Some(_)) => break,
                    (opcode, None) => (opcode, Vec::new()),
                };
                if data.len() + frame.payload.len() > MAX_MESSAGE {
                    break;
                }
                data.extend_from_slice(&frame.payload);
                if !frame.fin {
                    partial = Some((opcode, data));
                    continue;
                }
                let message = if opcode == 0x2 {
                    Incoming::Binary(data)
                } else {
                    match String::from_utf8(data) {
                        Ok(text) => Incoming::Text(text),
                        Err(_) => break,
                    }
                };
                if incoming.send(message).is_err() {
                    return;
                }
            }
            0x8 => {
                let code = (frame.payload.len() >= 2).then(|| u16::from_be_bytes([frame.payload[0], frame.payload[1]]));
                let _ = incoming.send(Incoming::Closed(code));
                let _ = outgoing.send(Outgoing::Close(code.unwrap_or(1000), String::new()));
                open.set(false);
                return;
            }
            0x9 => {
                if outgoing.send(Outgoing::Pong(frame.payload)).is_err() {
                    break;
                }
            }
            0xA => continue,
            _ => break,
        }
    }
    open.set(false);
    let _ = incoming.send(Incoming::Closed(None));
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

async fn read_frame(reader: &mut ReadHalf<Io>) -> std::io::Result<Frame> {
    let mut head = [0u8; 2];
    reader.read_exact(&mut head).await?;
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0f;
    if head[1] & 0x80 != 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "masked server frame"));
    }
    let length = match head[1] & 0x7f {
        126 => {
            let mut bytes = [0u8; 2];
            reader.read_exact(&mut bytes).await?;
            u16::from_be_bytes(bytes) as u64
        }
        127 => {
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes).await?;
            u64::from_be_bytes(bytes)
        }
        n => n as u64,
    };
    if length > MAX_MESSAGE as u64 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;
    Ok(Frame { fin, opcode, payload })
}

fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    payload
}

/// Writes one masked frame in one write.
async fn write_frame(writer: &mut WriteHalf<Io>, opcode: u8, payload: &[u8], state: &mut u32) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(14 + payload.len());
    head.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => head.push(0x80 | n as u8),
        n if n <= u16::MAX as usize => {
            head.push(0x80 | 126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(0x80 | 127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    let mask = state.to_be_bytes();
    head.extend_from_slice(&mask);
    head.extend(payload.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]));
    writer.write_all(&head).await?;
    writer.flush().await
}
