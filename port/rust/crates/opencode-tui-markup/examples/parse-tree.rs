//! Prints the tree a bundled grammar parses a file into (UTF-16, as the highlighter parses), for comparing parses
//! with web-tree-sitter: `parse-tree <grammar> <file>`.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let language = opencode_tui_markup::tree_sitter::language(opencode_tui_markup::tree_sitter::Grammar(
        Box::leak(args[1].clone().into_boxed_str()),
    ));
    let text: Vec<u16> = std::fs::read_to_string(&args[2]).unwrap().encode_utf16().collect();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse_utf16_le(&text, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
