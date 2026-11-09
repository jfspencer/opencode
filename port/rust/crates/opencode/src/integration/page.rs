//! The branded pages a local OAuth callback server answers with (packages/core/src/oauth/page.ts): self-contained
//! HTML, no external assets. The style sheet, the wordmark and the status icons are the reference's own text
//! (`page/*`, extracted from it).

const STYLES: &str = include_str!("page/styles.css");
const WORDMARK: &str = include_str!("page/wordmark.svg");
const ICON_SPINNER: &str = include_str!("page/spinner.svg");
const ICON_CHECK: &str = include_str!("page/check.svg");
const ICON_CROSS: &str = include_str!("page/cross.svg");
const AUTO_CLOSE_SCRIPT: &str = "setTimeout(function(){try{window.close()}catch(e){}},2500)";

/// `OauthCallbackPage.success`: the authorization finished (the window closes itself).
pub fn success(provider: Option<&str>) -> String {
    let message = match provider {
        Some(provider) => format!("OpenCode is now connected to {}.", escape(provider)),
        None => "OpenCode is now authorized.".to_owned(),
    };
    document(
        "Authorization successful",
        &card("success", "Authorization successful", &message, None, "You can close this window."),
        Some(AUTO_CLOSE_SCRIPT),
    )
}

/// `OauthCallbackPage.error`: the authorization failed, with what went wrong.
pub fn error(detail: &str, provider: Option<&str>) -> String {
    let message = match provider {
        Some(provider) => format!("OpenCode couldn't finish connecting to {}.", escape(provider)),
        None => "OpenCode couldn't complete authorization.".to_owned(),
    };
    document(
        "Authorization failed",
        &card("error", "Authorization failed", &message, Some(detail), "Close this window and try again from OpenCode."),
        None,
    )
}

fn card(status: &str, headline: &str, message: &str, detail: Option<&str>, footnote: &str) -> String {
    let detail = detail.map(trim_js).filter(|d| !d.is_empty());
    format!(
        "<main class=\"card\" id=\"oc-card\" data-status=\"{status}\" role=\"status\" aria-live=\"polite\">
      <div class=\"brand\">{WORDMARK}</div>
      <div class=\"status\" aria-hidden=\"true\">
        <span class=\"icon icon-pending\">{ICON_SPINNER}</span>
        <span class=\"icon icon-success\">{ICON_CHECK}</span>
        <span class=\"icon icon-error\">{ICON_CROSS}</span>
      </div>
      <h1 class=\"headline\" id=\"oc-headline\">{}</h1>
      <p class=\"message\" id=\"oc-message\">{message}</p>
      <pre class=\"detail\" id=\"oc-detail\"{}>{}</pre>
      <p class=\"footnote\" id=\"oc-footnote\">{}</p>
    </main>",
        escape(headline),
        if detail.is_some() { "" } else { " hidden" },
        detail.map(escape).unwrap_or_default(),
        escape(footnote),
    )
}

fn document(title: &str, body: &str, script: Option<&str>) -> String {
    format!(
        "<!doctype html>
<html lang=\"en\">
  <head>
    <meta charset=\"utf-8\" />
    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />
    <meta name=\"robots\" content=\"noindex\" />
    <title>{} · OpenCode</title>
    <style>{STYLES}</style>
  </head>
  <body>
    {body}{}
  </body>
</html>",
        escape(title),
        script.map(|script| format!("\n    <script>{script}</script>")).unwrap_or_default(),
    )
}

/// `String.prototype.trim` (JS whitespace, which includes the line terminators and the BOM).
fn trim_js(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

