//! The searches behind the glob and grep tools (packages/core/src/ripgrep.ts `glob` / `grep`, tool/search.ts): one
//! ripgrep run, its rows read as they arrive, stopped one row past the limit (so truncation is known without a
//! second search), bounded to 30 seconds.

use tokio::io::AsyncBufReadExt;

use crate::json;

const ERROR_BYTES: u64 = 8 * 1024;
const MAX_LINE_CHARS: usize = 2_000;
pub const DEFAULT_SEARCH_LIMIT: usize = 100;
const DEFAULT_SEARCH_TIMEOUT_MS: u64 = 30_000;

/// Why a search failed.
#[derive(Debug)]
pub enum SearchError {
    /// `Ripgrep.Error`: its message.
    Ripgrep(String),
    /// `Ripgrep.InvalidPatternError`: ripgrep's message.
    InvalidPattern(String),
    /// A model-facing failure (the timeout).
    Tool(String),
}

/// `normalizePath`: without leading `./` and `/`, with forward slashes.
fn normalize_path(value: &str) -> String {
    let mut rest = value;
    while let Some(stripped) = rest.strip_prefix("./").or_else(|| rest.strip_prefix(".\\")) {
        rest = stripped;
    }
    rest.trim_start_matches(['/', '\\']).replace('\\', "/")
}

/// `run`: up to `limit + 1` parsed rows (more means truncated), or the failure of the exit code.
async fn run<A>(
    cwd: &str,
    args: Vec<String>,
    limit: usize,
    pattern: Option<&str>,
    mut parse: impl FnMut(&str) -> Result<Option<A>, SearchError>,
) -> Result<Vec<A>, SearchError> {
    let binary = crate::fs::ripgrep().await.map_err(|e| SearchError::Ripgrep(e.message("ripgrep execution failed")))?;
    let mut child = tokio::process::Command::new(binary)
        .args(&args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| SearchError::Ripgrep("ripgrep execution failed".into()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = Vec::new();
        let _ = (&mut stderr).take(ERROR_BYTES).read_to_end(&mut buffer).await;
        String::from_utf8_lossy(&buffer).into_owned()
    });
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut rows = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).await.map_err(|_| SearchError::Ripgrep("ripgrep execution failed".into()))?;
        if read == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            continue;
        }
        if let Some(row) = parse(text)? {
            rows.push(row);
            if rows.len() > limit {
                let _ = child.kill().await;
                rows.truncate(limit);
                return Ok(rows);
            }
        }
    }
    let status = child.wait().await.map_err(|_| SearchError::Ripgrep("ripgrep execution failed".into()))?;
    let stderr = errors.await.unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if pattern.is_some() && code == 2 && (stderr.contains("regex parse error") || stderr.contains("error parsing regex")) {
        return Err(SearchError::InvalidPattern(crate::text::trim(&stderr).to_owned()));
    }
    if code != 0 && code != 1 && code != 2 {
        let message = crate::text::trim(&stderr);
        return Err(SearchError::Ripgrep(if message.is_empty() { format!("ripgrep failed with code {code}") } else { message.to_owned() }));
    }
    Ok(if code == 1 { vec![] } else { rows })
}

/// `ToolSearch.bounded`: one row past the limit asked for, the search bounded in time; the rows and whether there
/// were more.
pub async fn bounded<A, F>(limit: usize, search: impl FnOnce(usize) -> F) -> Result<(Vec<A>, bool), SearchError>
where
    F: std::future::Future<Output = Result<Vec<A>, SearchError>>,
{
    let timeout = std::time::Duration::from_millis(DEFAULT_SEARCH_TIMEOUT_MS);
    let mut items = match tokio::time::timeout(timeout, search(limit + 1)).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(SearchError::Tool(format!(
                "Search timed out after {} seconds. Consider using a more specific path or pattern.",
                DEFAULT_SEARCH_TIMEOUT_MS / 1_000
            )));
        }
    };
    let truncated = items.len() > limit;
    items.truncate(limit);
    Ok((items, truncated))
}

/// `Ripgrep.glob`: the files matching a glob under `cwd`, relative to it.
pub async fn glob(cwd: &str, pattern: &str, hidden: bool, limit: usize) -> Result<Vec<String>, SearchError> {
    let mut args = vec!["--no-config".to_owned(), "--files".to_owned()];
    if hidden {
        args.push("--hidden".into());
    }
    args.push(format!("--glob={pattern}"));
    // Positive globs override ripgrep's hidden-file filter; exclude before applying the limit.
    if !hidden {
        args.push("--glob=!**/.*".into());
    }
    args.push("--glob=!**/.git/**".into());
    args.push(".".into());
    match run(cwd, args, limit, Some(pattern), |line| Ok(Some(normalize_path(line)))).await {
        Err(SearchError::InvalidPattern(message)) => Err(SearchError::Ripgrep(message)),
        other => other,
    }
}

/// `FileSystem.Match`: a matching line.
#[derive(Debug, Clone)]
pub struct Match {
    pub path: String,
    pub line: i64,
    pub text: String,
}

pub struct GrepInput<'a> {
    pub cwd: &'a str,
    pub pattern: &'a str,
    pub file: Option<&'a str>,
    pub include: Option<&'a str>,
    pub literal: bool,
    pub case_sensitive: Option<bool>,
}

/// One `match` record of `rg --json`; other records are skipped.
fn parse_match(line: &str) -> Result<Option<Match>, SearchError> {
    let record = json::parse(line).ok_or_else(|| SearchError::Ripgrep("Invalid ripgrep JSON output".into()))?;
    if json::get_str(&record, "type") != Some("match") {
        return Ok(None);
    }
    let invalid = || SearchError::Ripgrep("Invalid ripgrep match output".into());
    let data = json::get(&record, "data").ok_or_else(invalid)?;
    let text_of = |key: &str| json::get(data, key).and_then(|v| json::get_str(v, "text")).map(str::to_owned);
    let path = text_of("path").ok_or_else(invalid)?;
    let lines = text_of("lines").ok_or_else(invalid)?;
    let integer = |key: &str, minimum: f64| json::get_num(data, key).filter(|n| n.fract() == 0.0 && *n >= minimum).map(|n| n as i64);
    let line_number = integer("line_number", 1.0).ok_or_else(invalid)?;
    integer("absolute_offset", 0.0).ok_or_else(invalid)?;
    // The submatches are decoded (a malformed one fails the search) but only the line is shown.
    for item in json::array(json::get(data, "submatches").ok_or_else(invalid)?).ok_or_else(invalid)? {
        json::get(item, "match").and_then(|m| json::get_str(m, "text")).ok_or_else(invalid)?;
        let number = |key: &str| json::get_num(item, key).filter(|n| n.fract() == 0.0 && *n >= 0.0);
        number("start").ok_or_else(invalid)?;
        number("end").ok_or_else(invalid)?;
    }
    let units: Vec<u16> = lines.encode_utf16().collect();
    let text = if units.len() > MAX_LINE_CHARS { format!("{}...", String::from_utf16_lossy(&units[..MAX_LINE_CHARS])) } else { lines };
    Ok(Some(Match { path: normalize_path(&path), line: line_number, text }))
}

/// `Ripgrep.grep`: the lines matching a pattern under `cwd` (or in one file of it).
pub async fn grep(input: &GrepInput<'_>, limit: usize) -> Result<Vec<Match>, SearchError> {
    let mut args = vec!["--no-config".to_owned(), "--json".into(), "--hidden".into(), "--no-messages".into()];
    if input.literal {
        args.push("--fixed-strings".into());
    }
    if input.case_sensitive == Some(false) {
        args.push("--ignore-case".into());
    }
    if let Some(include) = input.include {
        args.push(format!("--glob={include}"));
    }
    args.push("--glob=!**/.git/**".into());
    args.push("--".into());
    args.push(input.pattern.to_owned());
    args.push(input.file.unwrap_or(".").to_owned());
    run(input.cwd, args, limit, Some(input.pattern), parse_match).await
}

/// `Environment.typeFollowing`: the type of what a path names, a final symlink followed; `None` when missing.
fn type_following(path: &str) -> Option<&'static str> {
    let info = std::fs::metadata(path).ok()?;
    Some(if info.is_file() {
        "file"
    } else if info.is_dir() {
        "directory"
    } else {
        "other"
    })
}

/// `ToolSearch.pathType`: the type of the search path, or the failure naming a missing one.
pub fn path_type(absolute: &str, display: &str) -> Result<&'static str, String> {
    type_following(absolute).ok_or_else(|| format!("Search path does not exist: {display}"))
}

/// A permission request's metadata: the present entries (as `JSON` drops `undefined`).
pub fn metadata(entries: Vec<(&str, Option<opencode_protocol::Json>)>) -> opencode_protocol::Json {
    let mut out = json::object();
    for (key, value) in entries {
        if let Some(value) = value {
            json::set(&mut out, key, value);
        }
    }
    out
}
