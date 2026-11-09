# Generates src/tree_sitter/grammars.rs from the TUI parsers config dumped by list-parsers.ts:
#   bun harness/list-parsers.ts <reference>/packages/tui/src/parsers-config.ts > parsers.jsonl
#   python3 harness/gen-grammars.py parsers.jsonl src/tree_sitter/grammars.rs
import json, sys
config, out_path = sys.argv[1], sys.argv[2]
rows = [json.loads(l) for l in open(config)]
wasm_sym = {
    'tree-sitter-python.wasm': 'python', 'tree-sitter-rust.wasm': 'rust', 'tree-sitter-go.wasm': 'go',
    'tree-sitter-cpp.wasm': 'cpp', 'tree-sitter-c_sharp.wasm': 'c_sharp', 'tree-sitter-bash.wasm': 'bash',
    'tree-sitter-c.wasm': 'c', 'tree-sitter-java.wasm': 'java', 'tree-sitter-kotlin.wasm': 'kotlin',
    'tree-sitter-ruby.wasm': 'ruby', 'tree-sitter-php_only.wasm': 'php_only', 'tree-sitter-scala.wasm': 'scala',
    'tree-sitter-html.wasm': 'html', 'tree-sitter-vue.wasm': 'vue', 'tree-sitter-hcl.wasm': 'hcl',
    'tree-sitter-json.wasm': 'json', 'tree-sitter-yaml.wasm': 'yaml', 'tree-sitter-haskell.wasm': 'haskell',
    'tree-sitter-css.wasm': 'css', 'tree-sitter-julia.wasm': 'julia', 'tree-sitter-lua.wasm': 'lua',
    'tree-sitter-ocaml.wasm': 'ocaml', 'tree-sitter-clojure.wasm': 'clojure', 'tree-sitter-swift.wasm': 'swift',
    'tree-sitter-toml.wasm': 'toml', 'tree-sitter-nix.wasm': 'nix', 'tree-sitter-diff.wasm': 'diff',
    'tree-sitter-elixir.wasm': 'elixir', 'tree-sitter-fsharp.wasm': 'fsharp', 'tree-sitter-r.wasm': 'r',
    'tree-sitter-make.wasm': 'make', 'tree-sitter-vim.wasm': 'vim', 'tree-sitter-xml.wasm': 'xml',
    'tree-sitter-agda.wasm': 'agda'}
syms = ['javascript', 'typescript', 'tsx', 'markdown', 'markdown_inline', 'zig'] + [wasm_sym[r['wasm'].split('/')[-1]] for r in rows]
out = []
w = out.append
w("//! The bundled grammars (grammars/SOURCES.txt, compiled by build.rs) and queries (queries/SOURCES.txt): OpenTUI's")
w("//! default parsers (`default-parsers.ts`) and the TUI's (`packages/tui/src/parsers-config.ts`), with each query file")
w("//! as the TS reference fetches it. The TUI config's locals queries are dropped, as OpenTUI ignores them.")
w("//! Generated from parsers-config.ts; keep in step with it.")
w("")
w("use super::{FiletypeParserOptions, Grammar, InjectionMapping};")
w("")
w('unsafe extern "C" {')
for s in syms:
    w(f"    fn tree_sitter_{s}() -> *const tree_sitter::ffi::TSLanguage;")
w("}")
w("")
w("/// The language of a bundled grammar.")
w("pub fn language(grammar: Grammar) -> tree_sitter::Language {")
w("    let ptr = unsafe {")
w("        match grammar.0 {")
for s in syms:
    w(f'            "{s}" => tree_sitter_{s}(),')
w('            other => panic!("no bundled grammar {other}"),')
w("        }")
w("    };")
w("    unsafe { tree_sitter::Language::from_raw(ptr) }")
w("}")
w("")
w("/// Every bundled grammar.")
w("pub const GRAMMARS: &[&str] = &[" + ", ".join(f'"{s}"' for s in syms) + "];")
w("")
def q(ft, kind, n):
    return ", ".join(f'include_str!("../../queries/{ft}/{kind}-{i}.scm")' for i in range(n))
w("fn parser(")
w("    filetype: &str,")
w("    aliases: &[&str],")
w("    grammar: &'static str,")
w("    highlights: &[&'static str],")
w("    injections: &[&'static str],")
w(") -> FiletypeParserOptions {")
w("    FiletypeParserOptions {")
w("        filetype: filetype.to_owned(),")
w("        aliases: aliases.iter().map(|a| a.to_string()).collect(),")
w("        highlights: highlights.to_vec(),")
w("        injections: injections.to_vec(),")
w("        grammar: Grammar(grammar),")
w("        injection_mapping: None,")
w("    }")
w("}")
w("")
w("/// OpenTUI's default parsers (`getParsers()`), in order.")
w("pub fn default_parsers() -> Vec<FiletypeParserOptions> {")
w(f'    let mut markdown = parser("markdown", &[], "markdown", &[{q("markdown", "highlights", 1)}], &[{q("markdown", "injections", 1)}]);')
w("    markdown.injection_mapping = Some(InjectionMapping {")
w('        node_types: vec![("inline".into(), "markdown_inline".into()), ("pipe_table_cell".into(), "markdown_inline".into())],')
w("        info_string_map: [")
for k, v in [('javascript', 'javascript'), ('js', 'javascript'), ('jsx', 'javascriptreact'), ('javascriptreact', 'javascriptreact'), ('typescript', 'typescript'), ('ts', 'typescript'), ('tsx', 'typescriptreact'), ('typescriptreact', 'typescriptreact'), ('markdown', 'markdown'), ('md', 'markdown')]:
    w(f'            ("{k}", "{v}"),')
w("        ]")
w("        .iter()")
w("        .map(|(k, v)| (k.to_string(), v.to_string()))")
w("        .collect(),")
w("    });")
w("    vec![")
w(f'        parser("javascript", &["javascriptreact"], "javascript", &[{q("javascript", "highlights", 1)}], &[]),')
w(f'        parser("typescript", &[], "typescript", &[{q("typescript", "highlights", 1)}], &[]),')
w(f'        parser("typescriptreact", &[], "tsx", &[{q("typescriptreact", "highlights", 1)}], &[]),')
w("        markdown,")
w(f'        parser("markdown_inline", &[], "markdown_inline", &[{q("markdown_inline", "highlights", 1)}], &[]),')
w(f'        parser("zig", &[], "zig", &[{q("zig", "highlights", 1)}], &[]),')
w("    ]")
w("}")
w("")
w("/// The TUI's parsers (`parsers-config.ts`), which it passes to `addDefaultParsers`.")
w("pub fn tui_parsers() -> Vec<FiletypeParserOptions> {")
w("    vec![")
for r in rows:
    ft = r['filetype']
    al = r.get('aliases') or []
    nh = len(r['queries'].get('highlights', []))
    ni = len(r['queries'].get('injections', []) or [])
    sym = wasm_sym[r['wasm'].split('/')[-1]]
    als = "&[" + ", ".join(f'"{a}"' for a in al) + "]"
    w(f'        parser("{ft}", {als}, "{sym}", &[{q(ft, "highlights", nh)}], &[{q(ft, "injections", ni)}]),')
w("    ]")
w("}")
open(out_path, "w").write("\n".join(out) + "\n")
