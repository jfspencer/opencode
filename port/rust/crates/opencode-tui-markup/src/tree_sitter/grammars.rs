//! The bundled grammars (grammars/SOURCES.txt, compiled by build.rs) and queries (queries/SOURCES.txt): OpenTUI's
//! default parsers (`default-parsers.ts`) and the TUI's (`packages/tui/src/parsers-config.ts`), with each query file
//! as the TS reference fetches it. The TUI config's locals queries are dropped, as OpenTUI ignores them.
//! Generated from parsers-config.ts; keep in step with it.

use super::{FiletypeParserOptions, Grammar, InjectionMapping};

unsafe extern "C" {
    fn tree_sitter_javascript() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_typescript() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_tsx() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_markdown() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_markdown_inline() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_zig() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_python() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_rust() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_go() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_cpp() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_c_sharp() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_bash() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_c() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_java() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_kotlin() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_ruby() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_php_only() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_scala() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_html() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_vue() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_hcl() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_json() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_yaml() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_haskell() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_css() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_julia() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_lua() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_ocaml() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_clojure() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_swift() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_toml() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_nix() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_diff() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_elixir() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_fsharp() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_r() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_make() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_vim() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_xml() -> *const tree_sitter::ffi::TSLanguage;
    fn tree_sitter_agda() -> *const tree_sitter::ffi::TSLanguage;
}

/// The language of a bundled grammar.
pub fn language(grammar: Grammar) -> tree_sitter::Language {
    let ptr = unsafe {
        match grammar.0 {
            "javascript" => tree_sitter_javascript(),
            "typescript" => tree_sitter_typescript(),
            "tsx" => tree_sitter_tsx(),
            "markdown" => tree_sitter_markdown(),
            "markdown_inline" => tree_sitter_markdown_inline(),
            "zig" => tree_sitter_zig(),
            "python" => tree_sitter_python(),
            "rust" => tree_sitter_rust(),
            "go" => tree_sitter_go(),
            "cpp" => tree_sitter_cpp(),
            "c_sharp" => tree_sitter_c_sharp(),
            "bash" => tree_sitter_bash(),
            "c" => tree_sitter_c(),
            "java" => tree_sitter_java(),
            "kotlin" => tree_sitter_kotlin(),
            "ruby" => tree_sitter_ruby(),
            "php_only" => tree_sitter_php_only(),
            "scala" => tree_sitter_scala(),
            "html" => tree_sitter_html(),
            "vue" => tree_sitter_vue(),
            "hcl" => tree_sitter_hcl(),
            "json" => tree_sitter_json(),
            "yaml" => tree_sitter_yaml(),
            "haskell" => tree_sitter_haskell(),
            "css" => tree_sitter_css(),
            "julia" => tree_sitter_julia(),
            "lua" => tree_sitter_lua(),
            "ocaml" => tree_sitter_ocaml(),
            "clojure" => tree_sitter_clojure(),
            "swift" => tree_sitter_swift(),
            "toml" => tree_sitter_toml(),
            "nix" => tree_sitter_nix(),
            "diff" => tree_sitter_diff(),
            "elixir" => tree_sitter_elixir(),
            "fsharp" => tree_sitter_fsharp(),
            "r" => tree_sitter_r(),
            "make" => tree_sitter_make(),
            "vim" => tree_sitter_vim(),
            "xml" => tree_sitter_xml(),
            "agda" => tree_sitter_agda(),
            other => panic!("no bundled grammar {other}"),
        }
    };
    unsafe { tree_sitter::Language::from_raw(ptr) }
}

/// Every bundled grammar.
pub const GRAMMARS: &[&str] = &["javascript", "typescript", "tsx", "markdown", "markdown_inline", "zig", "python", "rust", "go", "cpp", "c_sharp", "bash", "c", "java", "kotlin", "ruby", "php_only", "scala", "html", "vue", "hcl", "json", "yaml", "haskell", "css", "julia", "lua", "ocaml", "clojure", "swift", "toml", "nix", "diff", "elixir", "fsharp", "r", "make", "vim", "xml", "agda"];

fn parser(
    filetype: &str,
    aliases: &[&str],
    grammar: &'static str,
    highlights: &[&'static str],
    injections: &[&'static str],
) -> FiletypeParserOptions {
    FiletypeParserOptions {
        filetype: filetype.to_owned(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        highlights: highlights.to_vec(),
        injections: injections.to_vec(),
        grammar: Grammar(grammar),
        injection_mapping: None,
    }
}

/// OpenTUI's default parsers (`getParsers()`), in order.
pub fn default_parsers() -> Vec<FiletypeParserOptions> {
    let mut markdown = parser("markdown", &[], "markdown", &[include_str!("../../queries/markdown/highlights-0.scm")], &[include_str!("../../queries/markdown/injections-0.scm")]);
    markdown.injection_mapping = Some(InjectionMapping {
        node_types: vec![("inline".into(), "markdown_inline".into()), ("pipe_table_cell".into(), "markdown_inline".into())],
        info_string_map: [
            ("javascript", "javascript"),
            ("js", "javascript"),
            ("jsx", "javascriptreact"),
            ("javascriptreact", "javascriptreact"),
            ("typescript", "typescript"),
            ("ts", "typescript"),
            ("tsx", "typescriptreact"),
            ("typescriptreact", "typescriptreact"),
            ("markdown", "markdown"),
            ("md", "markdown"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    });
    vec![
        parser("javascript", &["javascriptreact"], "javascript", &[include_str!("../../queries/javascript/highlights-0.scm")], &[]),
        parser("typescript", &[], "typescript", &[include_str!("../../queries/typescript/highlights-0.scm")], &[]),
        parser("typescriptreact", &[], "tsx", &[include_str!("../../queries/typescriptreact/highlights-0.scm")], &[]),
        markdown,
        parser("markdown_inline", &[], "markdown_inline", &[include_str!("../../queries/markdown_inline/highlights-0.scm")], &[]),
        parser("zig", &[], "zig", &[include_str!("../../queries/zig/highlights-0.scm")], &[]),
    ]
}

/// The TUI's parsers (`parsers-config.ts`), which it passes to `addDefaultParsers`.
pub fn tui_parsers() -> Vec<FiletypeParserOptions> {
    vec![
        parser("python", &[], "python", &[include_str!("../../queries/python/highlights-0.scm")], &[]),
        parser("rust", &[], "rust", &[include_str!("../../queries/rust/highlights-0.scm")], &[]),
        parser("go", &[], "go", &[include_str!("../../queries/go/highlights-0.scm")], &[]),
        parser("cpp", &[], "cpp", &[include_str!("../../queries/cpp/highlights-0.scm")], &[]),
        parser("csharp", &[], "c_sharp", &[include_str!("../../queries/csharp/highlights-0.scm")], &[]),
        parser("bash", &[], "bash", &[include_str!("../../queries/bash/highlights-0.scm")], &[]),
        parser("c", &[], "c", &[include_str!("../../queries/c/highlights-0.scm")], &[]),
        parser("java", &[], "java", &[include_str!("../../queries/java/highlights-0.scm")], &[]),
        parser("kotlin", &[], "kotlin", &[include_str!("../../queries/kotlin/highlights-0.scm")], &[]),
        parser("ruby", &[], "ruby", &[include_str!("../../queries/ruby/highlights-0.scm")], &[]),
        parser("php", &[], "php_only", &[include_str!("../../queries/php/highlights-0.scm")], &[]),
        parser("scala", &[], "scala", &[include_str!("../../queries/scala/highlights-0.scm")], &[]),
        parser("html", &[], "html", &[include_str!("../../queries/html/highlights-0.scm")], &[]),
        parser("vue", &[], "vue", &[include_str!("../../queries/vue/highlights-0.scm"), include_str!("../../queries/vue/highlights-1.scm")], &[]),
        parser("hcl", &[], "hcl", &[include_str!("../../queries/hcl/highlights-0.scm")], &[]),
        parser("json", &[], "json", &[include_str!("../../queries/json/highlights-0.scm")], &[]),
        parser("yaml", &[], "yaml", &[include_str!("../../queries/yaml/highlights-0.scm")], &[]),
        parser("haskell", &[], "haskell", &[include_str!("../../queries/haskell/highlights-0.scm")], &[]),
        parser("css", &[], "css", &[include_str!("../../queries/css/highlights-0.scm")], &[]),
        parser("julia", &[], "julia", &[include_str!("../../queries/julia/highlights-0.scm")], &[]),
        parser("lua", &[], "lua", &[include_str!("../../queries/lua/highlights-0.scm")], &[]),
        parser("ocaml", &[], "ocaml", &[include_str!("../../queries/ocaml/highlights-0.scm")], &[]),
        parser("clojure", &[], "clojure", &[include_str!("../../queries/clojure/highlights-0.scm")], &[]),
        parser("swift", &[], "swift", &[include_str!("../../queries/swift/highlights-0.scm")], &[]),
        parser("toml", &[], "toml", &[include_str!("../../queries/toml/highlights-0.scm")], &[]),
        parser("nix", &[], "nix", &[include_str!("../../queries/nix/highlights-0.scm")], &[]),
        parser("diff", &["udiff", "patch"], "diff", &[include_str!("../../queries/diff/highlights-0.scm")], &[]),
        parser("elixir", &[], "elixir", &[include_str!("../../queries/elixir/highlights-0.scm")], &[]),
        parser("fsharp", &[], "fsharp", &[include_str!("../../queries/fsharp/highlights-0.scm")], &[]),
        parser("r", &[], "r", &[include_str!("../../queries/r/highlights-0.scm")], &[]),
        parser("make", &["makefile"], "make", &[include_str!("../../queries/make/highlights-0.scm")], &[]),
        parser("vim", &[], "vim", &[include_str!("../../queries/vim/highlights-0.scm")], &[]),
        parser("xml", &[], "xml", &[include_str!("../../queries/xml/highlights-0.scm")], &[]),
        parser("agda", &[], "agda", &[include_str!("../../queries/agda/highlights-0.scm")], &[]),
    ]
}
