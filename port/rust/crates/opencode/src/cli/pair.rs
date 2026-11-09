//! `opencode pair` (packages/cli/src/commands/handlers/pair.ts): one-time links, and a QR code for the addresses
//! another device can reach, to connect a browser or app to the background service. `--remote` pairs through the
//! OpenTunnel address, turning remote access on first (after consent the first time).

use opencode_protocol::Json;

use super::clack::{Cancelled, Clack};
use super::client::Client;
use super::out::{Fail, Result, stderr, stdout};
use super::parse::Input;
use crate::json;

const REMOTE_WARNING: &str = "This will expose your OpenCode server to the internet.\nTraffic is end-to-end encrypted, and access requires your server password.\nTo pair locally instead, run `opencode pair` without the --remote flag.";
const NOT_ENABLED: &str = "Remote access was not enabled. To pair locally, run `opencode pair`.";

/// `pair --url`'s mapping: an HTTP(S) URL without credentials, query parameters or a fragment, as its `href` without
/// trailing slashes; `None` for anything else.
pub fn url(value: &str) -> Option<String> {
    let url = crate::url::Url::parse(value)?;
    let empty = |part: &Option<String>| part.as_deref().is_none_or(str::is_empty);
    if !matches!(url.scheme.as_str(), "http" | "https")
        || !url.username.is_empty()
        || !url.password.is_empty()
        || !empty(&url.query)
        || !empty(&url.fragment)
    {
        return None;
    }
    Some(url.href().trim_end_matches('/').to_owned())
}

/// `url.hostname` (an IPv6 host in brackets).
fn hostname(url: &str) -> String {
    crate::url::Url::parse(url).and_then(|u| u.host).unwrap_or_default()
}

fn loopback(hostname: &str) -> bool {
    hostname == "localhost" || hostname.ends_with(".localhost") || hostname.starts_with("127.") || hostname == "[::1]"
}

pub async fn command(input: &Input) -> Result {
    let config = super::service::read_config();
    if json::get(&config, "disabled") == Some(&Json::Bool(true)) {
        return Err(Fail::error("Pairing requires the background service; run `opencode service unset disabled` first"));
    }
    let remote = input.flag("remote");
    let given = input.str("url").map(str::to_owned);
    if remote && given.is_some() {
        return Err(Fail::error("--remote cannot be combined with --url"));
    }
    if remote && json::get(&config, "remote").is_none() {
        if let Some(code) = remote_consent(&config, input.flag("yes"))? {
            return Ok(code);
        }
        // Changing the setting restarts the service, and the ensure below starts it again with the tunnel.
        super::service::set("remote", "true", None).await?;
    }
    let endpoint = super::service::ensure_default(&mut |_| {}).await?;
    let client = Client::new(&endpoint);
    let urls: Vec<String> = if remote {
        vec![remote_url(&client).await?]
    } else if let Some(url) = &given {
        vec![url.clone()]
    } else {
        let info = client.call("server.info", &[], None, None).await?;
        json::get(&info, "urls").and_then(json::array).map(|items| items.iter().filter_map(json::as_str).map(str::to_owned).collect()).unwrap_or_default()
    };
    let pairing = client.call("server.pair", &[], None, None).await?;
    let code = json::get_str(&pairing, "code").unwrap_or("").to_owned();
    let expires = json::get_num(&pairing, "expires_in").unwrap_or(0.0);
    let links: Vec<String> = urls
        .iter()
        .map(|url| crate::url::Url::parse(url).and_then(|base| base.join(&format!("/auth/connect/{code}"))).map_or_else(String::new, |u| u.href()))
        .collect();
    // Loopback URLs are useless to the scanning device, so the QR code only carries reachable addresses.
    let reachable: Vec<&String> = urls.iter().filter(|url| !loopback(&hostname(url))).collect();
    let mut lines = vec![
        String::new(),
        format!("  Open a link to connect. Links work once and expire in {} minutes.", js_round(expires / 60.0)),
        String::new(),
    ];
    if links.is_empty() {
        lines.push("  (no server URLs)".into());
    } else {
        lines.extend(links.iter().map(|link| format!("  {link}")));
    }
    if !reachable.is_empty() {
        let mut payload = crate::json::Obj::new();
        payload.str("code", &code).set("urls", Json::Array(reachable.iter().map(|u| Json::String((*u).clone())).collect()));
        let qr = super::qr::render_unicode_compact(&json::text(&payload.done()), 2)
            .ok_or_else(|| Fail::named("RangeError", "Text is too long for a QR Code"))?;
        lines.push(String::new());
        lines.push(qr.split('\n').map(|line| format!("  {line}")).collect::<Vec<_>>().join("\n"));
    }
    lines.push(String::new());
    stdout(&format!("{}\n", lines.join("\n")));

    if remote || given.is_some() {
        return Ok(0);
    }
    let Some(url) = crate::url::Url::parse(&endpoint.url) else { return Ok(0) };
    let host = url.host.clone().unwrap_or_default();
    if !["localhost", "127.0.0.1", "[::1]"].contains(&host.as_str()) {
        return Ok(0);
    }
    let port = url.port.map(|p| p.to_string()).unwrap_or_default();
    stderr(
        &([
            "  Over SSH? Forward the port, then open the link on your machine:".to_owned(),
            format!("  ssh -L {port}:{host}:{port} <host>"),
            format!("  If port {port} is busy locally, forward another port and use it in the link."),
            String::new(),
            "  To connect from other devices, run `opencode service set hostname 0.0.0.0`.".into(),
            String::new(),
        ]
        .join("\n")
            + "\n"),
    );
    Ok(0)
}

/// `Math.round`.
fn js_round(value: f64) -> String {
    let rounded = (value + 0.5).floor();
    crate::json::text(&Json::Number(if rounded == 0.0 { 0.0 } else { rounded }))
}

/// `remoteConsent`: remote access is only explained and accepted the first time it is turned on. `None` lets it be
/// enabled; otherwise the exit code to stop with.
fn remote_consent(config: &Json, yes: bool) -> Result<Option<i32>> {
    if json::get(config, "remote").is_some() || json::get(config, "remoteAccepted") == Some(&Json::Bool(true)) {
        return Ok(None);
    }
    if !yes && !super::clack::interactive() {
        stderr(&format!("{REMOTE_WARNING}\nRun interactively or pass --yes to continue.\n"));
        return Ok(Some(1));
    }
    let clack = Clack::new();
    clack.warn(REMOTE_WARNING);
    if !yes {
        // The prompt blocks the (single) runtime thread while it reads keys.
        match clack.confirm("Expose your server to the internet?", true) {
            Err(Cancelled) => {
                clack.cancel(NOT_ENABLED);
                return Ok(Some(130));
            }
            Ok(false) => {
                clack.info(NOT_ENABLED);
                return Ok(Some(0));
            }
            Ok(true) => {}
        }
    }
    super::service::accept_remote()?;
    Ok(None)
}

/// `remoteURL`: the service attaches the tunnel in the background, so its URL is waited for in server info (every
/// second, for up to three minutes).
async fn remote_url(client: &Client) -> Result<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        if let Some(url) = tunnel_url(client).await {
            return Ok(url);
        }
        if std::time::Instant::now() + std::time::Duration::from_secs(1) > deadline {
            return Err(Fail::error(
                "Timed out waiting for the remote tunnel; run `opencode pair --remote` again to retry.\nBehind an HTTP proxy, give the background service its address with `opencode service set env HTTPS_PROXY <url>` (and `NO_PROXY` for hosts to reach directly).",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// The server-info URL on the route's tunnel hostname, once the service reports it.
async fn tunnel_url(client: &Client) -> Option<String> {
    let config = super::service::read_config();
    let route = json::get(&config, "remote").and_then(|r| json::get_str(r, "route"))?.to_owned();
    let expected = super::tunnel::hostname(&route).ok()??;
    let info = client.call("server.info", &[], None, None).await.ok()?;
    json::array(json::get(&info, "urls")?)?.iter().filter_map(json::as_str).find(|url| hostname(url) == expected).map(str::to_owned)
}
