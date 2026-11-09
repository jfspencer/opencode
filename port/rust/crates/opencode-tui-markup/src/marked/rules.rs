//! marked 17.0.1's `rules.ts`: the block and inline grammars, built with the same `edit()` source rewriting so
//! every regex has exactly the source marked compiles. Only the GFM grammars (`gfm: true`, `breaks: false`,
//! `pedantic: false`) are built: OpenTUI lexes with `{ gfm: true }` and the defaults.

use crate::js::{JsRegex, JsString, Replacement};
use std::sync::LazyLock;

/// A pattern being edited: `name` is replaced by `val` (minus carets) in the source.
struct Edit {
    source: JsString,
    flags: &'static str,
}

enum Name<'a> {
    /// A string name: only its first occurrence is replaced.
    Str(&'a str),
    /// A global regex name: every occurrence is replaced.
    Re(&'a str),
}

static CARET: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"(^|[^\[])\^", "g"));

fn edit(source: &str, flags: &'static str) -> Edit {
    Edit { source: JsString::from(source), flags }
}

impl Edit {
    fn replace(mut self, name: Name<'_>, val: &str) -> Edit {
        let val = CARET.replace_str(&JsString::from(val), "$1").to_string();
        // The value is a replacement pattern: `$` sequences expand as in String.prototype.replace.
        self.source = match name {
            Name::Str(s) => {
                let at = crate::js::JsStr::index_of(&self.source, &JsString::from(s), 0);
                if at < 0 {
                    self.source
                } else {
                    let re = JsRegex::compile(&regex_escape(s), "");
                    let _ = at;
                    re.replace(&self.source, Replacement::Pattern(&val))
                }
            }
            Name::Re(r) => JsRegex::compile(r, "g").replace(&self.source, Replacement::Pattern(&val)),
        };
        self
    }

    fn source(&self) -> String {
        self.source.to_string()
    }

    fn get_regex(&self) -> JsRegex {
        JsRegex::compile(&self.source(), self.flags)
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `rules.other`.
pub struct Other {
    pub code_remove_indent: JsRegex,
    pub output_link_replace: JsRegex,
    pub indent_code_compensation: JsRegex,
    pub beginning_space: JsRegex,
    pub ending_hash: JsRegex,
    pub starting_space_char: JsRegex,
    pub ending_space_char: JsRegex,
    pub non_space_char: JsRegex,
    pub new_line_char_global: JsRegex,
    pub tab_char_global: JsRegex,
    pub multiple_space_global: JsRegex,
    pub blank_line: JsRegex,
    pub double_blank_line: JsRegex,
    pub blockquote_start: JsRegex,
    pub blockquote_setext_replace: JsRegex,
    pub blockquote_setext_replace2: JsRegex,
    pub list_replace_tabs: JsRegex,
    pub list_is_task: JsRegex,
    pub list_replace_task: JsRegex,
    pub list_task_checkbox: JsRegex,
    pub any_line: JsRegex,
    pub href_brackets: JsRegex,
    pub table_delimiter: JsRegex,
    pub table_align_chars: JsRegex,
    pub table_row_blank_line: JsRegex,
    pub table_align_right: JsRegex,
    pub table_align_center: JsRegex,
    pub table_align_left: JsRegex,
    pub start_a_tag: JsRegex,
    pub end_a_tag: JsRegex,
    pub start_pre_script_tag: JsRegex,
    pub end_pre_script_tag: JsRegex,
    pub start_angle_bracket: JsRegex,
    pub end_angle_bracket: JsRegex,
    pub unicode_alpha_numeric: JsRegex,
    pub carriage_return: JsRegex,
    pub find_pipe: JsRegex,
    pub split_pipe: JsRegex,
    pub slash_pipe: JsRegex,
}

thread_local! {
    /// The regexes marked builds per list (`new RegExp` per call), compiled once per source as JavaScriptCore
    /// caches them.
    static DYNAMIC: std::cell::RefCell<std::collections::HashMap<(String, &'static str), std::rc::Rc<JsRegex>>> =
        Default::default();
}

fn cached(source: String, flags: &'static str) -> std::rc::Rc<JsRegex> {
    DYNAMIC.with(|cache| {
        cache
            .borrow_mut()
            .entry((source, flags))
            .or_insert_with_key(|(source, flags)| std::rc::Rc::new(JsRegex::compile(source, flags)))
            .clone()
    })
}

impl Other {
    /// `listItemRegex(bull)`.
    pub fn list_item_regex(&self, bull: &str) -> std::rc::Rc<JsRegex> {
        cached(format!(r"^( {{0,3}}{bull})((?:[\t ][^\n]*)?(?:\n|$))"), "")
    }

    fn indent_bound(indent: isize) -> isize {
        (3isize).min(indent - 1)
    }

    /// `nextBulletRegex(indent)`.
    pub fn next_bullet_regex(&self, indent: isize) -> std::rc::Rc<JsRegex> {
        cached(
            format!(r"^ {{0,{}}}(?:[*+-]|\d{{1,9}}[.)])((?:[ \t][^\n]*)?(?:\n|$))", Self::indent_bound(indent)),
            "",
        )
    }

    /// `hrRegex(indent)`.
    pub fn hr_regex(&self, indent: isize) -> std::rc::Rc<JsRegex> {
        cached(
            format!(r"^ {{0,{}}}((?:- *){{3,}}|(?:_ *){{3,}}|(?:\* *){{3,}})(?:\n+|$)", Self::indent_bound(indent)),
            "",
        )
    }

    /// `fencesBeginRegex(indent)`.
    pub fn fences_begin_regex(&self, indent: isize) -> std::rc::Rc<JsRegex> {
        cached(format!(r"^ {{0,{}}}(?:```|~~~)", Self::indent_bound(indent)), "")
    }

    /// `headingBeginRegex(indent)`.
    pub fn heading_begin_regex(&self, indent: isize) -> std::rc::Rc<JsRegex> {
        cached(format!(r"^ {{0,{}}}#", Self::indent_bound(indent)), "")
    }

    /// `htmlBeginRegex(indent)`.
    pub fn html_begin_regex(&self, indent: isize) -> std::rc::Rc<JsRegex> {
        cached(format!(r"^ {{0,{}}}<(?:[a-z].*>|!--)", Self::indent_bound(indent)), "i")
    }
}

pub static OTHER: LazyLock<Other> = LazyLock::new(|| Other {
    code_remove_indent: JsRegex::compile(r"^(?: {1,4}| {0,3}\t)", "gm"),
    output_link_replace: JsRegex::compile(r"\\([\[\]])", "g"),
    indent_code_compensation: JsRegex::compile(r"^(\s+)(?:```)", ""),
    beginning_space: JsRegex::compile(r"^\s+", ""),
    ending_hash: JsRegex::compile(r"#$", ""),
    starting_space_char: JsRegex::compile(r"^ ", ""),
    ending_space_char: JsRegex::compile(r" $", ""),
    non_space_char: JsRegex::compile(r"[^ ]", ""),
    new_line_char_global: JsRegex::compile(r"\n", "g"),
    tab_char_global: JsRegex::compile(r"\t", "g"),
    multiple_space_global: JsRegex::compile(r"\s+", "g"),
    blank_line: JsRegex::compile(r"^[ \t]*$", ""),
    double_blank_line: JsRegex::compile(r"\n[ \t]*\n[ \t]*$", ""),
    blockquote_start: JsRegex::compile(r"^ {0,3}>", ""),
    blockquote_setext_replace: JsRegex::compile(r"\n {0,3}((?:=+|-+) *)(?=\n|$)", "g"),
    blockquote_setext_replace2: JsRegex::compile(r"^ {0,3}>[ \t]?", "gm"),
    list_replace_tabs: JsRegex::compile(r"^\t+", ""),
    list_is_task: JsRegex::compile(r"^\[[ xX]\] +\S", ""),
    list_replace_task: JsRegex::compile(r"^\[[ xX]\] +", ""),
    list_task_checkbox: JsRegex::compile(r"\[[ xX]\]", ""),
    any_line: JsRegex::compile(r"\n.*\n", ""),
    href_brackets: JsRegex::compile(r"^<(.*)>$", ""),
    table_delimiter: JsRegex::compile(r"[:|]", ""),
    table_align_chars: JsRegex::compile(r"^\||\| *$", "g"),
    table_row_blank_line: JsRegex::compile(r"\n[ \t]*$", ""),
    table_align_right: JsRegex::compile(r"^ *-+: *$", ""),
    table_align_center: JsRegex::compile(r"^ *:-+: *$", ""),
    table_align_left: JsRegex::compile(r"^ *:-+ *$", ""),
    start_a_tag: JsRegex::compile(r"^<a ", "i"),
    end_a_tag: JsRegex::compile(r"^<\/a>", "i"),
    start_pre_script_tag: JsRegex::compile(r"^<(pre|code|kbd|script)(\s|>)", "i"),
    end_pre_script_tag: JsRegex::compile(r"^<\/(pre|code|kbd|script)(\s|>)", "i"),
    start_angle_bracket: JsRegex::compile(r"^<", ""),
    end_angle_bracket: JsRegex::compile(r">$", ""),
    unicode_alpha_numeric: JsRegex::compile(r"[\p{L}\p{N}]", "u"),
    carriage_return: JsRegex::compile(r"\r\n|\r", "g"),
    find_pipe: JsRegex::compile(r"\|", "g"),
    split_pipe: JsRegex::compile(r" \|", ""),
    slash_pipe: JsRegex::compile(r"\\\|", "g"),
});

/// The GFM block grammar (`block.gfm`).
pub struct Block {
    pub newline: JsRegex,
    pub code: JsRegex,
    pub fences: JsRegex,
    pub hr: JsRegex,
    pub heading: JsRegex,
    pub blockquote: JsRegex,
    pub list: JsRegex,
    pub html: JsRegex,
    pub def: JsRegex,
    pub table: JsRegex,
    pub lheading: JsRegex,
    pub paragraph: JsRegex,
    pub text: JsRegex,
}

/// The GFM inline grammar (`inline.gfm`).
pub struct Inline {
    pub backpedal: JsRegex,
    pub any_punctuation: JsRegex,
    pub autolink: JsRegex,
    pub block_skip: JsRegex,
    pub br: JsRegex,
    pub code: JsRegex,
    pub del: JsRegex,
    pub em_strong_l_delim: JsRegex,
    pub em_strong_r_delim_ast: JsRegex,
    pub em_strong_r_delim_und: JsRegex,
    pub escape: JsRegex,
    pub link: JsRegex,
    pub nolink: JsRegex,
    pub punctuation: JsRegex,
    pub reflink: JsRegex,
    pub reflink_search: JsRegex,
    pub tag: JsRegex,
    pub text: JsRegex,
    pub url: JsRegex,
}

const HR: &str = r"^ {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)";
const BULLET: &str = r"(?:[*+-]|\d{1,9}[.)])";
const BLOCK_CODE: &str = r"^((?: {4}| {0,3}\t)[^\n]+(?:\n(?:[ \t]*(?:\n|$))*)?)+";
const FENCES: &str = r"^ {0,3}(`{3,}(?=[^`\n]*(?:\n|$))|~{3,})([^\n]*)(?:\n|$)(?:|([\s\S]*?)(?:\n|$))(?: {0,3}\1[~`]* *(?=\n|$)|$)";
const HEADING: &str = r"^ {0,3}(#{1,6})(?=\s|$)(.*)(?:\n+|$)";
const LHEADING_CORE: &str = r"^(?!bull |blockCode|fences|blockquote|heading|html|table)((?:.|\n(?!\s*?\n|bull |blockCode|fences|blockquote|heading|html|table))+?)\n {0,3}(=+|-+) *(?:\n+|$)";
const PARAGRAPH: &str = r"^([^\n]+(?:\n(?!hr|heading|lheading|blockquote|fences|list|html|table| +\n)[^\n]+)*)";
const BLOCK_TEXT: &str = r"^[^\n]+";
const BLOCK_LABEL: &str = r"(?!\s*\])(?:\\[\s\S]|[^\[\]\\])+";
const TAG_NAMES: &str = concat!(
    "address|article|aside|base|basefont|blockquote|body|caption",
    "|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption",
    "|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe",
    "|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option",
    "|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title",
    "|tr|track|ul"
);
const COMMENT: &str = r"<!--(?:-?>|[\s\S]*?(?:-->|$))";

pub static BLOCK: LazyLock<Block> = LazyLock::new(|| {
    let hr = JsRegex::compile(HR, "");
    let lheading_gfm = edit(LHEADING_CORE, "")
        .replace(Name::Re("bull"), BULLET)
        .replace(Name::Re("blockCode"), r"(?: {4}| {0,3}\t)")
        .replace(Name::Re("fences"), r" {0,3}(?:`{3,}|~{3,})")
        .replace(Name::Re("blockquote"), r" {0,3}>")
        .replace(Name::Re("heading"), r" {0,3}#{1,6}")
        .replace(Name::Re("html"), r" {0,3}<[^\n>]+>\n")
        .replace(Name::Re("table"), r" {0,3}\|?(?:[:\- ]*\|)+[\:\- ]*\n")
        .get_regex();
    let def = edit(
        r#"^ {0,3}\[(label)\]: *(?:\n[ \t]*)?([^<\s][^\s]*|<.*?>)(?:(?: +(?:\n[ \t]*)?| *\n[ \t]*)(title))? *(?:\n+|$)"#,
        "",
    )
    .replace(Name::Str("label"), BLOCK_LABEL)
    .replace(Name::Str("title"), r#"(?:"(?:\\"?|[^"\\])*"|'[^'\n]*(?:\n[^'\n]+)*\n?'|\([^()]*\))"#)
    .get_regex();
    let list = edit(r"^( {0,3}bull)([ \t][^\n]+?)?(?:\n|$)", "").replace(Name::Re("bull"), BULLET).get_regex();
    let html = edit(
        concat!(
            r"^ {0,3}(?:",
            r"<(script|pre|style|textarea)[\s>][\s\S]*?(?:</\1>[^\n]*\n+|$)",
            r"|comment[^\n]*(\n+|$)",
            r"|<\?[\s\S]*?(?:\?>\n*|$)",
            r"|<![A-Z][\s\S]*?(?:>\n*|$)",
            r"|<!\[CDATA\[[\s\S]*?(?:\]\]>\n*|$)",
            "|</?(tag)(?: +|\\n|/?>)[\\s\\S]*?(?:(?:\\n[ \t]*)+\\n|$)",
            "|<(?!script|pre|style|textarea)([a-z][\\w-]*)(?:attribute)*? */?>(?=[ \\t]*(?:\\n|$))[\\s\\S]*?(?:(?:\\n[ \t]*)+\\n|$)",
            "|</(?!script|pre|style|textarea)[a-z][\\w-]*\\s*>(?=[ \\t]*(?:\\n|$))[\\s\\S]*?(?:(?:\\n[ \t]*)+\\n|$)",
            ")"
        ),
        "i",
    )
    .replace(Name::Str("comment"), COMMENT)
    .replace(Name::Str("tag"), TAG_NAMES)
    .replace(Name::Str("attribute"), r#" +[a-zA-Z:_][\w.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\s"'=<>`]+)?"#)
    .get_regex();
    let gfm_table = edit(
        concat!(
            "^ *([^\\n ].*)\\n",
            " {0,3}((?:\\| *)?:?-+:? *(?:\\| *:?-+:? *)*(?:\\| *)?)",
            "(?:\\n((?:(?! *\\n|hr|heading|blockquote|code|fences|list|html).*(?:\\n|$))*)\\n*|$)"
        ),
        "",
    )
    .replace(Name::Str("hr"), HR)
    .replace(Name::Str("heading"), " {0,3}#{1,6}(?:\\s|$)")
    .replace(Name::Str("blockquote"), " {0,3}>")
    .replace(Name::Str("code"), "(?: {4}| {0,3}\t)[^\\n]")
    .replace(Name::Str("fences"), " {0,3}(?:`{3,}(?=[^`\\n]*\\n)|~{3,})[^\\n]*\\n")
    .replace(Name::Str("list"), " {0,3}(?:[*+-]|1[.)]) ")
    .replace(Name::Str("html"), "</?(?:tag)(?: +|\\n|/?>)|<(?:script|pre|style|textarea|!--)")
    .replace(Name::Str("tag"), TAG_NAMES);
    let paragraph = edit(PARAGRAPH, "")
        .replace(Name::Str("hr"), HR)
        .replace(Name::Str("heading"), " {0,3}#{1,6}(?:\\s|$)")
        .replace(Name::Str("|lheading"), "")
        .replace(Name::Str("table"), &gfm_table.source())
        .replace(Name::Str("blockquote"), " {0,3}>")
        .replace(Name::Str("fences"), " {0,3}(?:`{3,}(?=[^`\\n]*\\n)|~{3,})[^\\n]*\\n")
        .replace(Name::Str("list"), " {0,3}(?:[*+-]|1[.)]) ")
        .replace(Name::Str("html"), "</?(?:tag)(?: +|\\n|/?>)|<(?:script|pre|style|textarea|!--)")
        .replace(Name::Str("tag"), TAG_NAMES)
        .get_regex();
    // blockquote embeds the normal (non-GFM) paragraph, as in rules.ts.
    let normal_paragraph = edit(PARAGRAPH, "")
        .replace(Name::Str("hr"), HR)
        .replace(Name::Str("heading"), " {0,3}#{1,6}(?:\\s|$)")
        .replace(Name::Str("|lheading"), "")
        .replace(Name::Str("|table"), "")
        .replace(Name::Str("blockquote"), " {0,3}>")
        .replace(Name::Str("fences"), " {0,3}(?:`{3,}(?=[^`\\n]*\\n)|~{3,})[^\\n]*\\n")
        .replace(Name::Str("list"), " {0,3}(?:[*+-]|1[.)]) ")
        .replace(Name::Str("html"), "</?(?:tag)(?: +|\\n|/?>)|<(?:script|pre|style|textarea|!--)")
        .replace(Name::Str("tag"), TAG_NAMES);
    let blockquote = edit(r"^( {0,3}> ?(paragraph|[^\n]*)(?:\n|$))+", "")
        .replace(Name::Str("paragraph"), &normal_paragraph.source())
        .get_regex();
    Block {
        newline: JsRegex::compile(r"^(?:[ \t]*(?:\n|$))+", ""),
        code: JsRegex::compile(BLOCK_CODE, ""),
        fences: JsRegex::compile(FENCES, ""),
        hr,
        heading: JsRegex::compile(HEADING, ""),
        blockquote,
        list,
        html,
        def,
        table: gfm_table.get_regex(),
        lheading: lheading_gfm,
        paragraph,
        text: JsRegex::compile(BLOCK_TEXT, ""),
    }
});

const PUNCTUATION: &str = r"[\p{P}\p{S}]";
const PUNCTUATION_OR_SPACE: &str = r"[\s\p{P}\p{S}]";
const NOT_PUNCTUATION_OR_SPACE: &str = r"[^\s\p{P}\p{S}]";
const PUNCTUATION_GFM_STRONG_EM: &str = r"(?!~)[\p{P}\p{S}]";
const PUNCTUATION_OR_SPACE_GFM_STRONG_EM: &str = r"(?!~)[\s\p{P}\p{S}]";
const NOT_PUNCTUATION_OR_SPACE_GFM_STRONG_EM: &str = r"(?:[^\s\p{P}\p{S}]|~)";
const INLINE_LABEL: &str = r"(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+[^`]*?`+(?!`)|[^\[\]\\`])*?";
const CASE_INSENSITIVE_PROTOCOL: &str = r"[hH][tT][tT][pP][sS]?|[fF][tT][pP]";

const EM_STRONG_R_DELIM_AST_CORE: &str = concat!(
    r"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)",
    r"|[^*]+(?=[^*])",
    r"|(?!\*)punct(\*+)(?=[\s]|$)",
    r"|notPunctSpace(\*+)(?!\*)(?=punctSpace|$)",
    r"|(?!\*)punctSpace(\*+)(?=notPunctSpace)",
    r"|[\s](\*+)(?!\*)(?=punct)",
    r"|(?!\*)punct(\*+)(?!\*)(?=punct)",
    r"|notPunctSpace(\*+)(?=notPunctSpace)"
);

pub static INLINE: LazyLock<Inline> = LazyLock::new(|| {
    let punctuation = edit(r"^((?![*_])punctSpace)", "u").replace(Name::Re("punctSpace"), PUNCTUATION_OR_SPACE).get_regex();
    // JavaScriptCore supports lookbehind, so precode- is `(?<!`)()`.
    let block_skip = edit(r"link|precode-code|html", "g")
        .replace(
            Name::Str("link"),
            r"\[(?:[^\[\]`]|(?<a>`+)[^`]+\k<a>(?!`))*?\]\((?:\\[\s\S]|[^\\\(\)]|\((?:\\[\s\S]|[^\\\(\)])*\))*\)",
        )
        .replace(Name::Str("precode-"), r"(?<!`)()")
        .replace(Name::Str("code"), r"(?<b>`+)[^`]+\k<b>(?!`)")
        .replace(Name::Str("html"), r"<(?! )[^<>]*?>")
        .get_regex();
    let em_strong_l_delim_gfm = edit(r"^(?:\*+(?:((?!\*)punct)|[^\s*]))|^_+(?:((?!_)punct)|([^\s_]))", "u")
        .replace(Name::Re("punct"), PUNCTUATION_GFM_STRONG_EM)
        .get_regex();
    let em_strong_r_delim_ast_gfm = edit(EM_STRONG_R_DELIM_AST_CORE, "gu")
        .replace(Name::Re("notPunctSpace"), NOT_PUNCTUATION_OR_SPACE_GFM_STRONG_EM)
        .replace(Name::Re("punctSpace"), PUNCTUATION_OR_SPACE_GFM_STRONG_EM)
        .replace(Name::Re("punct"), PUNCTUATION_GFM_STRONG_EM)
        .get_regex();
    let em_strong_r_delim_und = edit(
        concat!(
            r"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)",
            r"|[^_]+(?=[^_])",
            r"|(?!_)punct(_+)(?=[\s]|$)",
            r"|notPunctSpace(_+)(?!_)(?=punctSpace|$)",
            r"|(?!_)punctSpace(_+)(?=notPunctSpace)",
            r"|[\s](_+)(?!_)(?=punct)",
            r"|(?!_)punct(_+)(?!_)(?=punct)"
        ),
        "gu",
    )
    .replace(Name::Re("notPunctSpace"), NOT_PUNCTUATION_OR_SPACE)
    .replace(Name::Re("punctSpace"), PUNCTUATION_OR_SPACE)
    .replace(Name::Re("punct"), PUNCTUATION)
    .get_regex();
    let any_punctuation = edit(r"\\(punct)", "gu").replace(Name::Re("punct"), PUNCTUATION).get_regex();
    let autolink = edit(r"^<(scheme:[^\s\x00-\x1f<>]*|email)>", "")
        .replace(Name::Str("scheme"), r"[a-zA-Z][a-zA-Z0-9+.-]{1,31}")
        .replace(
            Name::Str("email"),
            r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+(@)[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(?![-_])",
        )
        .get_regex();
    let inline_comment = edit(COMMENT, "").replace(Name::Str("(?:-->|$)"), "-->");
    let tag = edit(
        concat!(
            "^comment",
            "|^</[a-zA-Z][\\w:-]*\\s*>",
            "|^<[a-zA-Z][\\w-]*(?:attribute)*?\\s*/?>",
            "|^<\\?[\\s\\S]*?\\?>",
            "|^<![a-zA-Z]+\\s[\\s\\S]*?>",
            "|^<!\\[CDATA\\[[\\s\\S]*?\\]\\]>"
        ),
        "",
    )
    .replace(Name::Str("comment"), &inline_comment.source())
    .replace(Name::Str("attribute"), r#"\s+[a-zA-Z:_][\w.:-]*(?:\s*=\s*"[^"]*"|\s*=\s*'[^']*'|\s*=\s*[^\s"'=<>`]+)?"#)
    .get_regex();
    let link = edit(r"^!?\[(label)\]\(\s*(href)(?:(?:[ \t]*(?:\n[ \t]*)?)(title))?\s*\)", "")
        .replace(Name::Str("label"), INLINE_LABEL)
        .replace(Name::Str("href"), r"<(?:\\.|[^\n<>\\])+>|[^ \t\n\x00-\x1f]*")
        .replace(Name::Str("title"), r#""(?:\\"?|[^"\\])*"|'(?:\\'?|[^'\\])*'|\((?:\\\)?|[^)\\])*\)"#)
        .get_regex();
    let reflink =
        edit(r"^!?\[(label)\]\[(ref)\]", "").replace(Name::Str("label"), INLINE_LABEL).replace(Name::Str("ref"), BLOCK_LABEL);
    let nolink = edit(r"^!?\[(ref)\](?:\[\])?", "").replace(Name::Str("ref"), BLOCK_LABEL);
    let reflink_search = edit(r"reflink|nolink(?!\()", "g")
        .replace(Name::Str("reflink"), &reflink.source())
        .replace(Name::Str("nolink"), &nolink.source())
        .get_regex();
    let url = edit(r"^((?:protocol):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\s<]*|^email", "")
        .replace(Name::Str("protocol"), CASE_INSENSITIVE_PROTOCOL)
        .replace(Name::Str("email"), r"[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])")
        .get_regex();
    let text = edit(
        r"^([`~]+|[^`~])(?:(?= {2,}\n)|(?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)|[\s\S]*?(?:(?=[\\<!\[`*~_]|\b_|protocol:\/\/|www\.|$)|[^ ](?= {2,}\n)|[^a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-](?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)))",
        "",
    )
    .replace(Name::Str("protocol"), CASE_INSENSITIVE_PROTOCOL)
    .get_regex();
    Inline {
        backpedal: JsRegex::compile(r#"(?:[^?!.,:;*_'"~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'"~)]+(?!$))+"#, ""),
        any_punctuation,
        autolink,
        block_skip,
        br: JsRegex::compile(r"^( {2,}|\\)\n(?!\s*$)", ""),
        code: JsRegex::compile(r"^(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)", ""),
        del: JsRegex::compile(r"^(~~?)(?=[^\s~])((?:\\[\s\S]|[^\\])*?(?:\\[\s\S]|[^\s~\\]))\1(?=[^~]|$)", ""),
        em_strong_l_delim: em_strong_l_delim_gfm,
        em_strong_r_delim_ast: em_strong_r_delim_ast_gfm,
        em_strong_r_delim_und,
        escape: JsRegex::compile(r##"^\\([!"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])"##, ""),
        link,
        nolink: nolink.get_regex(),
        punctuation,
        reflink: reflink.get_regex(),
        reflink_search,
        tag,
        text,
        url,
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    /// The sources marked 17.0.1 compiles, printed from the reference (`Lexer.rules`).
    #[test]
    fn sources_compile() {
        let _ = &BLOCK.paragraph;
        let _ = &INLINE.text;
        assert_eq!(
            BLOCK.list.source,
            r"^( {0,3}(?:[*+-]|\d{1,9}[.)]))([ \t][^\n]+?)?(?:\n|$)"
        );
    }
}
