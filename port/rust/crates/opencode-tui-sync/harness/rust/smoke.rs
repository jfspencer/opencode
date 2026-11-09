//! A live check of the client over the network: connects the contexts to a running server, syncs the location and
//! its catalogs, creates a session and waits for its events, printing what the store holds.
//!
//! usage: sync-smoke <server url> <password> <directory>

use std::rc::Rc;
use std::time::Duration;

use opencode_tui_sync::client::{basic_auth, Client, ClientOptions};
use opencode_tui_sync::connection::Status;
use opencode_tui_sync::context::client::{ClientContext, ClientInput};
use opencode_tui_sync::context::data::DataContext;
use opencode_tui_sync::data::CreateInput;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::reactive::Inert;
use opencode_tui_sync::transport::HttpTransport;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (url, password, directory) = (args[1].clone(), args[2].clone(), args[3].clone());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let api = Client::make(ClientOptions {
            base_url: url.clone(),
            headers: vec![basic_auth("opencode", &password)],
            transport: Rc::new(HttpTransport),
        });
        let client = ClientContext::new(ClientInput {
            api,
            url: Some(url),
            headers: None,
            service: None,
            log: Some(Rc::new(|level, message, tags| eprintln!("log {level:?} {message} {}", opencode_tui_sync::client::stringify(tags)))),
            reactivity: Rc::new(Inert),
        });
        let data = DataContext::new(&client, &directory);
        for _ in 0..200 {
            if client.connection().status() == Status::Connected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        println!("connection: {:?}", client.connection().status());
        data.location.sync(None).wait().await.expect("location sync");
        let info = data.location.info(None).expect("location info");
        println!("location: {}", opencode_tui_sync::client::stringify(&info));
        println!("agents: {}", data.location.agent.list(None).map(|a| a.arr_len()).unwrap_or(0));
        let created = data.session.create(CreateInput { title: Some("smoke".into()), ..Default::default() });
        let session = created.request.wait().await.expect("session create");
        println!("created: {} {}", created.id, session.str("title").unwrap_or(""));
        tokio::time::sleep(Duration::from_millis(500)).await;
        data.session.sync(&created.id, false).wait().await.expect("session sync");
        let stored = data.session.get(&created.id).expect("stored session");
        println!("stored: {} title={:?} family={:?}", stored.str("id").unwrap_or(""), stored.str("title"), data.session.family(&created.id));
        data.session.message.sync(&created.id).wait().await.expect("message sync");
        println!("messages: {}", data.session.message.list(&created.id).len());
        println!("connection: {:?} attempt {}", client.connection().status(), client.connection().attempt());
        client.dispose();
    });
}

trait Len {
    fn arr_len(&self) -> usize;
}

impl Len for Rc<opencode_protocol::Json> {
    fn arr_len(&self) -> usize {
        match &**self {
            opencode_protocol::Json::Array(items) => items.len(),
            _ => 0,
        }
    }
}
