//! The Rust side of the theme and config harness (harness/run.sh): reads one JSON case per line on stdin and prints
//! `{"id": .., "out": ..}` per case, in the shape harness/theme.ts and harness/config.ts print for the TS reference.

use std::io::{BufRead, Write};

use opencode_protocol::Json;
use opencode_protocol::wire::Object;
use opencode_tui::theme::registry::ThemeRegistry;
use opencode_tui::theme::resolve::{parse_theme_document, resolve_theme_document};
use opencode_tui::theme::rgba::RGBA;
use opencode_tui::theme::select::{Mode, theme_modes};
use opencode_tui::theme::syntax::generate_syntax;
use opencode_tui::theme::system::{TerminalColors, generate_system, terminal_mode};
use opencode_tui::theme::value::{Rec, Value};
use opencode_tui::theme::{ResolvedTheme, color, v1};

fn obj(entries: Vec<(&str, Json)>) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn s(text: &str) -> Json {
    Json::String(text.to_owned())
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.as_object().and_then(|o| o.get(key)).unwrap_or(&NULL)
}

fn str_of(value: &Json) -> &str {
    match value {
        Json::String(s) => s,
        _ => "",
    }
}

fn mode_of(value: &Json) -> Mode {
    Mode::parse(str_of(value)).unwrap_or(Mode::Light)
}

fn syntax_json(theme: &ResolvedTheme) -> Json {
    Json::Array(
        generate_syntax(theme)
            .into_iter()
            .map(|rule| {
                let mut style = vec![("foreground", s(&rule.style.foreground.unwrap().to_hex()))];
                if let Some(bg) = rule.style.background {
                    style.push(("background", s(&bg.to_hex())));
                }
                for (flag, on) in [("bold", rule.style.bold), ("italic", rule.style.italic), ("underline", rule.style.underline)] {
                    if on {
                        style.push((flag, Json::Bool(true)));
                    }
                }
                obj(vec![("scope", Json::Array(rule.scope.iter().map(|x| s(x)).collect())), ("style", obj(style))])
            })
            .collect(),
    )
}

/// decrease(1), increase(1), decrease(2.7), increase(-3) of every token, tokens in sorted path order.
fn shifts(theme: &ResolvedTheme) -> Json {
    let mut colors = theme.colors();
    colors.sort_by(|(a, _), (b, _)| {
        let a: Vec<&str> = a.split('.').collect();
        let b: Vec<&str> = b.split('.').collect();
        a.iter().map(|x| x.encode_utf16().collect::<Vec<_>>()).cmp(b.iter().map(|x| x.encode_utf16().collect::<Vec<_>>()))
    });
    Json::Array(
        colors
            .into_iter()
            .map(|(_, c)| {
                let all = [theme.decrease(c, 1.0), theme.increase(c, 1.0), theme.decrease(c, 2.7), theme.increase(c, -3.0)];
                s(&all.iter().map(|x| x.to_hex()).collect::<Vec<_>>().join(" "))
            })
            .collect(),
    )
}

fn views(theme: &ResolvedTheme) -> Vec<(&'static str, Json)> {
    use opencode_tui::theme::SurfaceName;
    vec![
        ("base", theme.to_json()),
        ("dialog", theme.surface(SurfaceName::Dialog).to_json()),
        ("footer", theme.surface(SurfaceName::DialogFooter).to_json()),
        ("shifts", shifts(theme)),
        ("syntax", syntax_json(theme)),
    ]
}

fn modes_json(modes: &[Mode]) -> Json {
    Json::Array(modes.iter().map(|m| s(m.as_str())).collect())
}

fn resolved(document: &Rec) -> Result<Json, String> {
    let modes = theme_modes(document);
    let mut out = vec![("modes", modes_json(&modes))];
    for mode in &modes {
        let theme = resolve_theme_document(document, Some(*mode)).map_err(|e| e.message)?;
        out.push((mode.as_str(), obj(views(&theme))));
    }
    Ok(obj(out))
}

fn guard(result: Result<Json, String>) -> Json {
    match result {
        Ok(value) => obj(vec![("ok", value)]),
        Err(message) => obj(vec![("error", s(&message))]),
    }
}

fn loaded(registry: &ThemeRegistry, source: &opencode_tui::theme::ThemeSource, name: &str, mode: Mode, new_designs: bool) -> Result<Json, String> {
    let loaded = registry.load_theme(source, name, mode, new_designs).map_err(|e| e.message)?;
    let mut out = vec![("modes", modes_json(&loaded.modes)), ("mode", s(loaded.mode.as_str()))];
    out.extend(views(&loaded.theme));
    Ok(obj(out))
}

fn run_theme(registry: &mut ThemeRegistry, case: &Json) -> Json {
    let new_designs = matches!(get(case, "newDesigns"), Json::Bool(true));
    match str_of(get(case, "kind")) {
        "builtin" => {
            let name = str_of(get(case, "name"));
            guard((|| {
                let sources = registry.available(new_designs).map_err(|e| e.message)?;
                let source = sources.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).ok_or("no such theme")?;
                loaded(registry, &source, name, mode_of(get(case, "mode")), new_designs)
            })())
        }
        "document" => guard((|| {
            let document = parse_theme_document(&Value::from_json(get(case, "input")), str_of(get(case, "name"))).map_err(|e| e.message)?;
            resolved(&document)
        })()),
        "v1" => guard((|| {
            let input = Value::from_json(get(case, "input"));
            let rec = input.as_rec().cloned().unwrap_or_default();
            let document = if new_designs { v1::new_designs::migrate_v1_new_designs(&rec) } else { v1::migrate_v1(&rec) }.map_err(|e| e.message)?;
            resolved(&document)
        })()),
        "system" => {
            let c = get(case, "colors");
            let opt = |v: &Json| match v {
                Json::String(s) => Some(s.clone()),
                _ => None,
            };
            let colors = TerminalColors {
                palette: match get(c, "palette") {
                    Json::Array(items) => items.iter().map(opt).collect(),
                    _ => Vec::new(),
                },
                default_foreground: opt(get(c, "defaultForeground")),
                default_background: opt(get(c, "defaultBackground")),
            };
            let mode = mode_of(get(case, "mode"));
            guard((|| {
                let source = std::rc::Rc::new(generate_system(&colors, mode.as_str()).map_err(|e| e.message)?);
                let out = loaded(registry, &source, "system", mode, new_designs)?;
                let Json::Object(mut o) = out else { unreachable!() };
                let mut first = Object::new();
                first.insert("mode".into(), terminal_mode(&colors).map(s).unwrap_or(Json::Null));
                for (k, v) in o.drain(..) {
                    first.insert(k, v);
                }
                Ok(Json::Object(first))
            })())
        }
        "tint" => {
            let n = |v: &Json| if let Json::Number(n) = v { *n } else { 0.0 };
            let out = color::tint(RGBA::from_hex(str_of(get(case, "base"))), RGBA::from_hex(str_of(get(case, "overlay"))), n(get(case, "alpha")));
            s(&out.to_hex())
        }
        _ => Json::Null,
    }
}

// --- config cases ----------------------------------------------------------------------------------------------

use opencode_tui::config::file::{CliConfig, parse_record};
use opencode_tui::config::jsonc::{self, FormattingOptions, Segment};
use opencode_tui::config::keybind::{Binding, DEFINITIONS, default_value};
use opencode_tui::config::resolve::{ResolveOptions, resolve, to_json};
use opencode_tui::config::schema::decode_info;

fn bindings_json(bindings: &[Binding]) -> Json {
    Json::Array(bindings.iter().map(|b| Json::Object(b.0.clone())).collect())
}

/// Walks to the parent of `path`, replacing non-objects on the way with `{}` (as the harness's TS mutation does).
fn parent_of<'a>(draft: &'a mut Json, path: &[String]) -> &'a mut Object {
    if !matches!(draft, Json::Object(_)) {
        *draft = Json::Object(Object::new());
    }
    let Json::Object(target) = draft else { unreachable!() };
    let mut target: &mut Object = target;
    for key in &path[..path.len() - 1] {
        let entry = target.entry(key.clone()).or_insert(Json::Null);
        if !matches!(entry, Json::Object(_)) {
            *entry = Json::Object(Object::new());
        }
        let Json::Object(next) = entry else { unreachable!() };
        target = next;
    }
    target
}

fn apply_ops(draft: &mut Json, ops: &[Json]) {
    for op in ops {
        let path: Vec<String> = match get(op, "path") {
            Json::Array(items) => items.iter().map(|i| str_of(i).to_owned()).collect(),
            _ => continue,
        };
        let last = path.last().unwrap().clone();
        let parent = parent_of(draft, &path);
        match str_of(get(op, "op")) {
            "set" => {
                parent.insert(last, get(op, "value").clone());
            }
            "delete" => {
                parent.shift_remove(&last);
            }
            _ => {
                let mut spread = match parent.get(&last) {
                    Some(Json::Object(o)) => o.clone(),
                    _ => Object::new(),
                };
                spread.insert(str_of(get(op, "key")).to_owned(), get(op, "value").clone());
                parent.insert(last, Json::Object(spread));
            }
        }
    }
}

fn run_config(case: &Json, id: &Json) -> Json {
    match str_of(get(case, "kind")) {
        "definitions" => Json::Array(
            DEFINITIONS
                .iter()
                .map(|(name, _, description)| Json::Array(vec![s(name), default_value(name).unwrap(), s(description)]))
                .collect(),
        ),
        "decode" => decode_info(get(case, "input")).map(|v| s(&opencode_protocol::to_string(&v))).unwrap_or(Json::Null),
        "resolve" => {
            let mut info = decode_info(get(case, "input")).unwrap_or(Json::Object(Object::new()));
            if let Json::Array(unknown) = get(case, "unknown")
                && !unknown.is_empty()
            {
                let Json::Object(object) = &mut info else { unreachable!() };
                let mut keybinds = match object.get("keybinds") {
                    Some(Json::Object(k)) => k.clone(),
                    _ => Object::new(),
                };
                for name in unknown {
                    keybinds.insert(str_of(name).to_owned(), s("x"));
                }
                object.insert("keybinds".into(), Json::Object(keybinds));
            }
            let options = ResolveOptions {
                terminal_suspend: matches!(get(case, "terminalSuspend"), Json::Bool(true)),
                herdr_env: matches!(get(case, "herdr"), Json::Bool(true)).then(|| "1".to_owned()),
            };
            guard((|| {
                let resolved = resolve(&info, &options)?;
                let keybinds = &resolved.keybinds;
                let sample = ["app.exit", "session.list", "prompt.paste", "nope"];
                let gather = keybinds.gather("g", &["prompt.paste", "app.exit", "nope", "session.new"]);
                let again = keybinds.gather("g", &["session.list"]);
                Ok(obj(vec![
                    ("resolved", to_json(&resolved)),
                    ("get", Json::Array(sample.iter().map(|n| bindings_json(keybinds.get(n))).collect())),
                    ("has", Json::Array(sample.iter().map(|n| Json::Bool(keybinds.has(n))).collect())),
                    ("gather", bindings_json(&gather)),
                    ("again", bindings_json(&again)),
                    ("pick", bindings_json(&keybinds.pick("g", &["session.new", "app.exit"]))),
                    ("omit", bindings_json(&keybinds.omit("g", &["app.exit"]))),
                    ("omitMany", bindings_json(&keybinds.omit("g", &["app.exit", "session.new"]))),
                    ("missing", bindings_json(&keybinds.pick("h", &["app.exit"]))),
                ]))
            })())
        }
        "parse" => {
            let (value, errors) = jsonc::parse(str_of(get(case, "text")));
            if errors {
                obj(vec![("errors", Json::Bool(true))])
            } else {
                obj(vec![("value", value.map(|v| s(&opencode_protocol::to_string(&v))).unwrap_or(Json::Null))])
            }
        }
        "modify" => {
            let text = str_of(get(case, "text"));
            let path: Vec<Segment> = match get(case, "path") {
                Json::Array(items) => items
                    .iter()
                    .map(|i| match i {
                        Json::Number(n) => Segment::Index(*n as i64),
                        other => Segment::Key(str_of(other).to_owned()),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let remove = matches!(get(case, "remove"), Json::Bool(true));
            let value = get(case, "value");
            let options = FormattingOptions { tab_size: 2, insert_spaces: true };
            guard((|| {
                let edits = jsonc::modify(text, &path, if remove { None } else { Some(value) }, options)?;
                jsonc::apply_edits(text, &edits).map(|t| s(&t))
            })())
        }
        "service" => {
            let work = std::path::PathBuf::from(std::env::var("OPENCODE_TUI_WORK").expect("OPENCODE_TUI_WORK"));
            let directory = work.join(format!("rust-{}", opencode_protocol::to_string(id)));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            if let Json::String(file) = get(case, "file") {
                std::fs::write(directory.join("cli.json"), file).unwrap();
            }
            let content = match get(case, "content") {
                Json::String(text) if !text.is_empty() => parse_record(text).and_then(|r| decode_info(&r)),
                _ => None,
            };
            let config = CliConfig::with_content(&directory, &directory, content);
            let result = match get(case, "ops") {
                Json::Array(ops) => match config.update(|draft| apply_ops(draft, ops)) {
                    Ok(value) => ("ok", value),
                    Err(error) => ("error", s(&error.to_string())),
                },
                _ => ("ok", config.get()),
            };
            let text = std::fs::read_to_string(directory.join("cli.json")).map(|t| s(&t)).unwrap_or(Json::Null);
            let _ = std::fs::remove_dir_all(&directory);
            obj(vec![result, ("text", text)])
        }
        _ => Json::Null,
    }
}

fn main() {
    let mut registry = ThemeRegistry::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.is_empty() {
            continue;
        }
        let input = opencode_protocol::wire::json::parse(&line).expect("case JSON");
        let case = get(&input, "case");
        let out = match str_of(get(&input, "group")) {
            "theme" => run_theme(&mut registry, case),
            "config" => run_config(case, get(&input, "id")),
            _ => Json::Null,
        };
        let result = obj(vec![("id", get(&input, "id").clone()), ("out", out)]);
        writeln!(stdout, "{}", opencode_protocol::to_string(&result)).unwrap();
    }
}
