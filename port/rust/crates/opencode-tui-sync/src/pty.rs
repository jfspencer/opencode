//! Persistent terminals (packages/client/src/solid/pty.ts): the server's messages on a terminal socket, the input
//! frames sent to it, and the socket's URL.
//!
//! `createPersistentPtyClient(api, { url }).connect(input)` asks for a connect ticket and opens a WebSocket; here
//! [`PersistentPtyClient::connect_url`] does the first part and gives the `ws(s)://` URL to open.
//! TODO(port): a WebSocket client for the terminal panes (the TS one is the runtime's `WebSocket`).

use opencode_protocol::Json;

use crate::client::{Client, Error, RequestOptions};
use crate::data::Promise;
use crate::js;
use crate::json::JsonExt;
use crate::obj;
use crate::runtime::Deferred;

/// `PersistentPtyMessage`: the fields clients read of each message.
#[derive(Clone, Debug, PartialEq)]
pub enum PersistentPtyMessage {
    Attached { input_protocol: Option<f64>, cols: f64, rows: f64, role: Option<String> },
    ReplayComplete,
    Resized { cols: f64, rows: f64, checkpoint: String },
    ControllerChanged { attachment_id: Option<Option<String>> },
    Exited { exit_code: Option<Option<f64>> },
    TitleChanged { title: String },
    ForegroundProcessChanged { process: Option<String> },
}

impl PersistentPtyMessage {
    /// The decoded value as TS's schema decode outputs it (declared fields only, in declaration order).
    pub fn to_json(&self) -> Json {
        let null_or = |value: &Option<Option<String>>| value.as_ref().map(|v| v.clone().map(Json::String).unwrap_or(Json::Null));
        match self {
            Self::Attached { input_protocol, cols, rows, role } => obj! {
                "type" => "attached",
                "inputProtocol" => *input_protocol,
                "info" => obj! { "size" => obj! { "cols" => *cols, "rows" => *rows } },
                "role" => role.clone(),
            },
            Self::ReplayComplete => obj! { "type" => "replay_complete" },
            Self::Resized { cols, rows, checkpoint } => {
                obj! { "type" => "resized", "cols" => *cols, "rows" => *rows, "checkpoint" => checkpoint.as_str() }
            }
            Self::ControllerChanged { attachment_id } => obj! { "type" => "controller_changed", "attachmentID" => null_or(attachment_id) },
            Self::Exited { exit_code } => obj! {
                "type" => "exited",
                "exitCode" => exit_code.map(|v| v.map(Json::Number).unwrap_or(Json::Null)),
            },
            Self::TitleChanged { title } => obj! { "type" => "title_changed", "title" => title.as_str() },
            Self::ForegroundProcessChanged { process } => obj! {
                "type" => "foreground_process_changed",
                "process" => process.clone().map(Json::String).unwrap_or(Json::Null),
            },
        }
    }
}

/// `decodePersistentPtyMessage(text)`: `None` for anything the schema rejects.
pub fn decode_persistent_pty_message(text: &str) -> Option<PersistentPtyMessage> {
    let value = opencode_protocol::wire::parse(text).ok()?;
    if !matches!(value, Json::Object(_)) {
        return None;
    }
    let number = |key: &str| match value.get(key) {
        Some(Json::Number(n)) => Some(*n),
        _ => None,
    };
    // `Schema.optional(X)`: absent, or an X.
    let optional_number = |key: &str| -> Option<Option<f64>> {
        match value.get(key) {
            None => Some(None),
            Some(Json::Number(n)) => Some(Some(*n)),
            Some(_) => None,
        }
    };
    let positive_int = |v: Option<&Json>| match v {
        Some(Json::Number(n)) if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 && *n > 0.0 => Some(*n),
        _ => None,
    };
    match value.str("type")? {
        "attached" => {
            let input_protocol = optional_number("inputProtocol")?;
            let size = value.get("info").filter(|i| matches!(i, Json::Object(_)))?.get("size").filter(|s| matches!(s, Json::Object(_)))?;
            let cols = positive_int(size.get("cols"))?;
            let rows = positive_int(size.get("rows"))?;
            let role = match value.get("role") {
                None => None,
                Some(Json::String(role)) if role == "controller" || role == "observer" => Some(role.clone()),
                Some(_) => return None,
            };
            Some(PersistentPtyMessage::Attached { input_protocol, cols, rows, role })
        }
        "replay_complete" => Some(PersistentPtyMessage::ReplayComplete),
        "resized" => Some(PersistentPtyMessage::Resized {
            cols: number("cols")?,
            rows: number("rows")?,
            checkpoint: value.str("checkpoint")?.to_owned(),
        }),
        "controller_changed" => {
            let attachment_id = match value.get("attachmentID") {
                None => None,
                Some(Json::Null) => Some(None),
                Some(Json::String(id)) => Some(Some(id.clone())),
                Some(_) => return None,
            };
            Some(PersistentPtyMessage::ControllerChanged { attachment_id })
        }
        "exited" => {
            let exit_code = match value.get("exitCode") {
                None => None,
                Some(Json::Null) => Some(None),
                Some(Json::Number(n)) => Some(Some(*n)),
                Some(_) => return None,
            };
            Some(PersistentPtyMessage::Exited { exit_code })
        }
        "title_changed" => Some(PersistentPtyMessage::TitleChanged { title: value.str("title")?.to_owned() }),
        "foreground_process_changed" => {
            let process = match value.get("process") {
                Some(Json::Null) => None,
                Some(Json::String(p)) => Some(p.clone()),
                _ => return None,
            };
            Some(PersistentPtyMessage::ForegroundProcessChanged { process })
        }
        _ => None,
    }
}

/// `ToUint16(value)`, as `DataView.setUint16` stores it.
fn uint16(value: f64) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    (value.trunc().rem_euclid(65_536.0)) as u16
}

/// `persistentPtyFrame(size, data)`: an input frame (1 and the bytes) or a resize-only frame (0).
pub fn persistent_pty_frame(cols: f64, rows: f64, data: Option<&[u8]>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + data.map_or(0, <[u8]>::len));
    frame.push(u8::from(data.is_some()));
    frame.extend_from_slice(&uint16(cols).to_be_bytes());
    frame.extend_from_slice(&uint16(rows).to_be_bytes());
    if let Some(data) = data {
        frame.extend_from_slice(data);
    }
    frame
}

/// `PersistentPtyConnectInput`.
#[derive(Clone, Debug)]
pub struct PersistentPtyConnectInput {
    pub pty_id: String,
    pub cursor: f64,
    pub attachment_id: String,
    pub takeover: bool,
}

/// `createPersistentPtyClient(api, { url })`.
#[derive(Clone)]
pub struct PersistentPtyClient {
    api: Client,
    url: String,
}

impl PersistentPtyClient {
    pub fn new(api: Client, url: impl Into<String>) -> Self {
        Self { api, url: url.into() }
    }

    /// `connect(input)` up to the socket: the connect ticket, then the URL to open.
    pub fn connect_url(&self, input: &PersistentPtyConnectInput) -> Promise<String> {
        let call = self.api.api().experimental().persistent_pty().connect_token(
            &obj! { "ptyID" => input.pty_id.as_str(), "x-opencode-ticket" => "1" },
            RequestOptions::default(),
        );
        let base = self.url.clone();
        let input = input.clone();
        Deferred::spawn(async move {
            let token = call.await?.json();
            let ticket = token.get("ticket").map(js::string).unwrap_or_else(|| "undefined".into());
            let mut url = url::Url::parse(&base).map_err(|e| Error::Other(crate::abort::Reason { name: "TypeError".into(), message: e.to_string() }))?;
            if !url.path().ends_with('/') {
                let path = format!("{}/", url.path());
                url.set_path(&path);
            }
            let mut url = url
                .join(&format!("api/experimental/persistent-pty/{}/connect", js::encode_uri_component(&input.pty_id)))
                .map_err(|e| Error::Other(crate::abort::Reason { name: "TypeError".into(), message: e.to_string() }))?;
            url.query_pairs_mut()
                .append_pair("ticket", &ticket)
                .append_pair("cursor", &js::number(input.cursor))
                .append_pair("attachment_id", &input.attachment_id)
                .append_pair("takeover", if input.takeover { "true" } else { "false" })
                .append_pair("input_protocol", "1");
            let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
            let _ = url.set_scheme(scheme);
            Ok(url.to_string())
        })
    }
}
