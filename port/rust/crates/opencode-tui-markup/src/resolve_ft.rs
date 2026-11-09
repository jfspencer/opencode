//! `lib/tree-sitter/resolve-ft.ts`: filetypes from extensions, basenames, paths and code fence info strings.

use crate::js::{JsStr, JsString, JsRegex};
use std::sync::LazyLock;

/// `extensionToFiletype`.
pub const EXTENSION_TO_FILETYPE: &[(&str, &str)] = &[
    ("astro", "astro"),
    ("bash", "bash"),
    ("c", "c"),
    ("cc", "cpp"),
    ("cjs", "javascript"),
    ("clj", "clojure"),
    ("cljs", "clojure"),
    ("cljc", "clojure"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("cs", "csharp"),
    ("cts", "typescript"),
    ("ctsx", "typescriptreact"),
    ("dart", "dart"),
    ("diff", "diff"),
    ("edn", "clojure"),
    ("go", "go"),
    ("gemspec", "ruby"),
    ("groovy", "groovy"),
    ("h", "c"),
    ("handlebars", "handlebars"),
    ("hbs", "handlebars"),
    ("hpp", "cpp"),
    ("hxx", "cpp"),
    ("h++", "cpp"),
    ("hh", "cpp"),
    ("hrl", "erlang"),
    ("hs", "haskell"),
    ("htm", "html"),
    ("html", "html"),
    ("ini", "ini"),
    ("js", "javascript"),
    ("jsx", "javascriptreact"),
    ("jl", "julia"),
    ("json", "json"),
    ("ksh", "bash"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("latex", "latex"),
    ("less", "less"),
    ("lua", "lua"),
    ("markdown", "markdown"),
    ("md", "markdown"),
    ("mdown", "markdown"),
    ("mkd", "markdown"),
    ("mjs", "javascript"),
    ("ml", "ocaml"),
    ("mli", "ocaml"),
    ("mts", "typescript"),
    ("mtsx", "typescriptreact"),
    ("patch", "diff"),
    ("php", "php"),
    ("pl", "perl"),
    ("pm", "perl"),
    ("ps1", "powershell"),
    ("psm1", "powershell"),
    ("py", "python"),
    ("pyi", "python"),
    ("r", "r"),
    ("rb", "ruby"),
    ("rake", "ruby"),
    ("rs", "rust"),
    ("ru", "ruby"),
    ("sass", "sass"),
    ("sc", "scala"),
    ("scala", "scala"),
    ("scss", "scss"),
    ("sh", "bash"),
    ("sql", "sql"),
    ("svelte", "svelte"),
    ("swift", "swift"),
    ("ts", "typescript"),
    ("tsx", "typescriptreact"),
    ("tex", "latex"),
    ("toml", "toml"),
    ("vue", "vue"),
    ("vim", "vim"),
    ("xml", "xml"),
    ("xsl", "xsl"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("zig", "zig"),
    ("zon", "zig"),
    ("zsh", "bash"),
    ("c++", "cpp"),
    ("erl", "erlang"),
    ("exs", "elixir"),
    ("ex", "elixir"),
    ("elm", "elm"),
    ("fsharp", "fsharp"),
    ("fs", "fsharp"),
    ("fsx", "fsharp"),
    ("fsscript", "fsharp"),
    ("fsi", "fsharp"),
    ("java", "java"),
    ("css", "css"),
];

/// `basenameToFiletype`.
pub const BASENAME_TO_FILETYPE: &[(&str, &str)] = &[
    (".bash_aliases", "bash"),
    (".bash_logout", "bash"),
    (".bash_profile", "bash"),
    (".bashrc", "bash"),
    (".kshrc", "bash"),
    (".profile", "bash"),
    (".vimrc", "vim"),
    (".zlogin", "bash"),
    (".zlogout", "bash"),
    (".zprofile", "bash"),
    (".zshenv", "bash"),
    (".zshrc", "bash"),
    ("appfile", "ruby"),
    ("berksfile", "ruby"),
    ("brewfile", "ruby"),
    ("cheffile", "ruby"),
    ("containerfile", "dockerfile"),
    ("dockerfile", "dockerfile"),
    ("fastfile", "ruby"),
    ("gemfile", "ruby"),
    ("gnumakefile", "make"),
    ("gvimrc", "vim"),
    ("guardfile", "ruby"),
    ("makefile", "make"),
    ("podfile", "ruby"),
    ("rakefile", "ruby"),
    ("thorfile", "ruby"),
    ("vagrantfile", "ruby"),
];

fn lookup(table: &[(&str, &'static str)], key: &JsString) -> Option<&'static str> {
    table.iter().find(|(k, _)| *key == **k).map(|(_, v)| *v)
}

static LEADING_DOT: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"^\.", ""));
static WHITESPACE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"\s+", ""));

/// `normalizeFiletypeToken(value)`.
fn normalize_filetype_token(value: &JsString) -> Option<JsString> {
    let v = LEADING_DOT.replace_str(&value.js_trim(), "").to_lower();
    if v.is_empty() { None } else { Some(v) }
}

/// `path.posix.basename(p)`: the last segment after trailing slashes are dropped.
fn posix_basename(p: &[u16]) -> JsString {
    let mut end = p.len();
    while end > 0 && p[end - 1] == b'/' as u16 {
        end -= 1;
    }
    if end == 0 {
        return JsString::new();
    }
    let start = p[..end].iter().rposition(|&c| c == b'/' as u16).map_or(0, |i| i + 1);
    JsString(p[start..end].to_vec())
}

/// `getBasename(value)`.
fn get_basename(value: &JsString) -> Option<JsString> {
    let v: Vec<u16> = value.js_trim().iter().map(|&c| if c == b'\\' as u16 { b'/' as u16 } else { c }).collect();
    if v.is_empty() {
        return None;
    }
    let base = posix_basename(&v).to_lower();
    if base.is_empty() { None } else { Some(base) }
}

/// `extToFiletype(extension)`.
pub fn ext_to_filetype(extension: &str) -> Option<String> {
    ext_to_filetype_js(&JsString::from(extension)).map(str::to_owned)
}

fn ext_to_filetype_js(extension: &JsString) -> Option<&'static str> {
    lookup(EXTENSION_TO_FILETYPE, &normalize_filetype_token(extension)?)
}

/// `pathToFiletype(path)`.
pub fn path_to_filetype(path: &str) -> Option<String> {
    path_to_filetype_js(&JsString::from(path)).map(str::to_owned)
}

fn path_to_filetype_js(path: &JsString) -> Option<&'static str> {
    let base = get_basename(path)?;
    if let Some(ft) = lookup(BASENAME_TO_FILETYPE, &base) {
        return Some(ft);
    }
    let last_dot = base.last_index_of(&[b'.' as u16]);
    if last_dot == -1 || last_dot as usize == base.len() - 1 {
        return None;
    }
    ext_to_filetype_js(&base.js_substring(last_dot + 1, None))
}

/// `infoStringToFiletype(infoString)`: a code fence's language.
pub fn info_string_to_filetype(info_string: &str) -> Option<String> {
    let info = JsString::from(info_string);
    let trimmed = info.js_trim();
    // `split(/\s+/, 1)[0]`
    let token = match WHITESPACE.find_from(&trimmed, 0) {
        Some(m) => trimmed.js_slice(0, Some(m.start as isize)),
        None => trimmed,
    };
    if let Some(ft) = lookup(BASENAME_TO_FILETYPE, &token.to_lower()) {
        return Some(ft.to_owned());
    }
    let normalized = normalize_filetype_token(&token)?;
    if let Some(ft) = lookup(BASENAME_TO_FILETYPE, &normalized) {
        return Some(ft.to_owned());
    }
    if let Some(ft) = path_to_filetype_js(&normalized) {
        return Some(ft.to_owned());
    }
    if let Some(ft) = ext_to_filetype_js(&normalized) {
        return Some(ft.to_owned());
    }
    Some(normalized.to_string())
}
