//! Compiles the tree-sitter grammars of grammars/SOURCES.txt into one static library each, plus
//! grammars/wctype.c, the wide-character classification of the WASM grammars the TS reference runs. Every
//! grammar gets wctype.h force-included so its scanner calls those functions rather than the C library's.
//!
//! The grammar sources are fetched material, not in git: port/scripts/vendor-grammars.sh puts them in
//! port/.vendor/tree-sitter (or the directory `OPENCODE_TREE_SITTER_VENDOR` names).
//!
//! Grammars build at -O2 in every profile: their code is generated tables that compile no faster without
//! optimisation, and a debug build should not lex at -O0 speed.

use std::path::{Path, PathBuf};

/// (C symbol suffix of `tree_sitter_<name>`, source directory under grammars/).
pub const GRAMMARS: &[(&str, &str)] = &[
    ("javascript", "javascript/src"),
    ("typescript", "typescript/typescript/src"),
    ("tsx", "typescript/tsx/src"),
    ("markdown", "markdown/tree-sitter-markdown/src"),
    ("markdown_inline", "markdown/tree-sitter-markdown-inline/src"),
    ("zig", "zig/src"),
    ("python", "python/src"),
    ("rust", "rust/src"),
    ("go", "go/src"),
    ("cpp", "cpp/src"),
    ("c_sharp", "csharp/src"),
    ("bash", "bash/src"),
    ("c", "c/src"),
    ("java", "java/src"),
    ("kotlin", "kotlin/src"),
    ("ruby", "ruby/src"),
    ("php_only", "php/php_only/src"),
    ("scala", "scala/src"),
    ("html", "html/src"),
    ("vue", "vue/src"),
    ("hcl", "hcl/src"),
    ("json", "json/src"),
    ("yaml", "yaml/src"),
    ("haskell", "haskell/src"),
    ("css", "css/src"),
    ("julia", "julia/src"),
    ("lua", "lua/src"),
    ("ocaml", "ocaml/grammars/ocaml/src"),
    ("clojure", "clojure/src"),
    ("swift", "swift/src"),
    ("toml", "toml/src"),
    ("nix", "nix/src"),
    ("diff", "diff/src"),
    ("elixir", "elixir/src"),
    ("fsharp", "fsharp/fsharp/src"),
    ("r", "r/src"),
    ("make", "make/src"),
    ("vim", "vim/src"),
    ("xml", "xml/xml/src"),
    ("agda", "agda/src"),
];

fn build(name: &str, src: &Path, wctype: &Path) {
    let mut build = cc::Build::new();
    build
        .include(src)
        .opt_level(2)
        .debug(false)
        .warnings(false)
        .flag_if_supported("-std=c11")
        .flag("-include")
        .flag(wctype.to_str().expect("UTF-8 path"))
        .define("TREE_SITTER_HIDE_SYMBOLS", None)
        .file(src.join("parser.c"));
    let scanner = src.join("scanner.c");
    if scanner.exists() {
        build.file(scanner);
    }
    build.compile(&format!("tree-sitter-{name}"));
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let own = manifest.join("grammars");
    let wctype = own.join("wctype.h");
    let grammars = match std::env::var_os("OPENCODE_TREE_SITTER_VENDOR") {
        Some(dir) => PathBuf::from(dir),
        // crates/opencode-tui-markup → port/.vendor/tree-sitter
        None => manifest.ancestors().nth(3).expect("the crate lies under port/rust/crates").join(".vendor/tree-sitter"),
    };
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=grammars");
    println!("cargo:rerun-if-env-changed=OPENCODE_TREE_SITTER_VENDOR");
    println!("cargo:rerun-if-changed={}", grammars.join("SOURCES.txt").display());

    let fetch = "run port/scripts/vendor-grammars.sh";
    let vendored = std::fs::read_to_string(grammars.join("SOURCES.txt"))
        .unwrap_or_else(|_| panic!("no tree-sitter grammar sources in {}: {fetch}", grammars.display()));
    let wanted = std::fs::read_to_string(own.join("SOURCES.txt")).expect("grammars/SOURCES.txt");
    if vendored != wanted {
        panic!("the tree-sitter grammar sources in {} are not those of grammars/SOURCES.txt: {fetch}", grammars.display());
    }
    for (name, dir) in GRAMMARS {
        if !grammars.join(dir).join("parser.c").exists() {
            panic!("the tree-sitter grammar {name} is missing from {}: {fetch}", grammars.display());
        }
    }

    cc::Build::new()
        .file(own.join("wctype.c"))
        .define("OPENCODE_MARKUP_WCTYPE_IMPL", None)
        .opt_level(2)
        .warnings(false)
        .compile("opencode-markup-wctype");

    let jobs: usize = std::env::var("NUM_JOBS").ok().and_then(|n| n.parse().ok()).unwrap_or(4).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(GRAMMARS.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((name, dir)) = GRAMMARS.get(index) else { break };
                    build(name, &grammars.join(dir), &wctype);
                }
            });
        }
    });
}
