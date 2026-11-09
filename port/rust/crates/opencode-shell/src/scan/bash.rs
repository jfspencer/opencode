//! The POSIX-family half of `scan.ts` (`scan` and every `bash*` / `scanBash*` helper), function for function.
//!
//! Commands live in an arena (`Ctx::arena`) and lists hold their ids: TS shares `Command` objects between the list
//! that owns them and the heredoc that later rewrites their resource. Heredoc lists are shared the same way between a
//! group and its parent. The regular expressions of the TS source are hand-written matchers below the scanner; each
//! names the regex it implements.

use super::{Command, Dialect, MAX_INPUT_LENGTH, MAX_SUBSTITUTION_DEPTH, Reason, Scan};
use crate::js::{self, JsString, at, len, set_has, set_has_or_empty, slice, starts_with_ascii, units};

type Res<T> = Result<T, Reason>;

const BASH_REDIRECTS: &[&str] = &["&>>", "&>", "<<<", "<<-", "<<", "<>", "<&", ">&", ">|", ">>", ">", "<"];
const BASH_LIST_OPERATORS: &[&str] = &["&&", "||", "|&"];
const ZSH_LIST_OPERATORS: &[&str] = &["&&", "||", "|&", "&!", "&|"];
const BASH_CASE_OPERATORS: &[&str] = &[";;&", ";;", ";&", ";|", "&&", "||", "|&"];
const ZSH_CASE_OPERATORS: &[&str] = &[";;&", ";;", ";&", ";|", "&&", "||", "|&", "&!", "&|"];
// Builtins that bind their operands to variables or evaluate them as variable names.
const BASH_BINDING_BUILTINS: &[&str] = &[
    "declare", "typeset", "local", "export", "readonly", "private", "unset", "read", "getln", "vared", "getopts",
    "mapfile", "readarray", "compgen", "zparseopts", "zregexparse", "zformat", "zstyle", "zstat", "stat",
];
const BASH_ARITHMETIC_COMPARISONS: &[&str] = &["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
const BASH_ASSIGNMENT_DECLARATIONS: &[&str] = &["declare", "typeset", "export", "readonly", "local"];
const BASH_DECLARATIONS: &[&str] = &["declare", "typeset", "export", "readonly", "local", "unset", "unsetenv"];
const BASH_PRECOMMANDS: &[&str] = &["builtin", "command", "time", "coproc", "!"];
const ZSH_PRECOMMANDS: &[&str] = &["noglob", "nocorrect", "exec", "-"];
const ZSH_ARITHMETIC_BUILTINS: &[&str] =
    &["integer", "float", "exit", "return", "shift", "break", "continue", "bye", "logout"];
const BASH_NON_FUNCTION_KEYWORDS: &[&str] =
    &["if", "for", "select", "case", "then", "elif", "else", "fi", "do", "done", "in", "esac"];
// Characters that can begin a quoting or expansion unit.
const BASH_UNIT_STARTS: &str = "\\$'\"`<>";
const COMPOUND_KEYWORDS: &[&str] = &["if", "while", "until", "for", "select", "case", "repeat"];

fn has(set: &[&str], word: &[u16]) -> bool {
    set.iter().any(|item| js::eq_ascii(word, item))
}

fn has_opt(set: &[&str], word: Option<&[u16]>) -> bool {
    word.is_some_and(|word| has(set, word))
}

fn word_at(words: &[JsString], index: isize) -> Option<&[u16]> {
    if index < 0 { None } else { words.get(index as usize).map(|w| w.as_slice()) }
}

fn from(words: &[JsString], index: isize) -> &[JsString] {
    let start = index.clamp(0, words.len() as isize) as usize;
    &words[start..]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Eval {
    Arithmetic,
    Binding,
    Deferred,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    Paren,
    Brace,
    Nofork,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    If,
    While,
    Until,
    For,
    Case,
    Repeat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Header,
    Condition,
    Pattern,
    Body,
    Do,
}

struct Structure {
    kind: Kind,
    phase: Phase,
    count: i64,
    saw_else: bool,
    saw_in: bool,
    pattern_started: bool,
    parenthesized: bool,
}

struct Heredoc {
    delimiter: JsString,
    dash_delimiter: Option<JsString>,
    quoted: bool,
    tabs: bool,
    binding: bool,
    owned: bool,
    command: Option<usize>,
    start: Option<isize>,
}

/// A word's cooked text, the literal text it decodes to apart from its expansions, and the literal text of its
/// command substitutions' words, which stands in for their output.
#[derive(Default, Clone)]
struct Text {
    word: JsString,
    literal: JsString,
    output: JsString,
}

// Shared across one scan: the work budget, the shell dialect, and whether a bound expansion reaches a variable sink.
struct Ctx {
    remaining: i64,
    dialect: Dialect,
    bound_dollar: bool,
    bound_open: bool,
    bound_bracket: bool,
    sink_bracket: bool,
    variable_sink: bool,
    eval_sink: bool,
    arena: Vec<Command>,
    heredocs: Vec<Vec<Heredoc>>,
}

struct State<'i, 'o> {
    input: &'i [u16],
    commands: Vec<usize>,
    nested_commands: Vec<usize>,
    words: Vec<JsString>,
    raw_words: Vec<JsString>,
    // Pending heredocs, shared with nested groups: a body starts after the next newline token at any nesting.
    heredocs: usize,
    structures: Vec<Structure>,
    text: Text,
    word_started: bool,
    word_start: isize,
    word_end: isize,
    command_start: Option<isize>,
    redirect_word_count: Option<isize>,
    redirect_prefix_end: Option<isize>,
    // Posix shells such as Dash read `&>` as `&` and `>`, so later words would start another command there.
    ampersand_redirect_words: Option<isize>,
    command_word_index: isize,
    assignment_word: bool,
    assignment_head_unsafe: bool,
    invalid: Option<Reason>,
    redirect_target: bool,
    dup_redirect: bool,
    here_string: bool,
    has_redirect: bool,
    has_file_redirect: bool,
    compound_end: bool,
    // A list operator (&&, ||, |, |&) still awaits its right-hand command.
    dangling: bool,
    // Receives each word's literal text when the list is a command substitution, whose output it stands in for.
    outer: Option<&'o mut Text>,
}

impl State<'_, '_> {
    fn last_structure(&self) -> Option<&Structure> {
        self.structures.last()
    }
}

fn invalid(state: &mut State, reason: Reason) {
    state.invalid.get_or_insert(reason);
}

fn push_command(ctx: &mut Ctx, command: Command) -> usize {
    ctx.arena.push(command);
    ctx.arena.len() - 1
}

pub fn scan(input: &[u16], dialect: Dialect) -> Scan {
    if len(input) > MAX_INPUT_LENGTH {
        return Scan::Opaque(Reason::InvalidStructure);
    }
    let mut ctx = Ctx {
        remaining: (MAX_INPUT_LENGTH * MAX_SUBSTITUTION_DEPTH) as i64,
        dialect,
        bound_dollar: false,
        bound_open: false,
        bound_bracket: false,
        sink_bracket: false,
        variable_sink: false,
        eval_sink: false,
        arena: Vec::new(),
        heredocs: Vec::new(),
    };
    let commands = match scan_bash(input, 0, 0, &mut ctx, None, None, None) {
        Err(reason) => return Scan::Opaque(reason),
        Ok((commands, _)) => commands,
    };
    if ctx.bound_dollar
        && ctx.bound_open
        && (ctx.eval_sink || ((ctx.bound_bracket || ctx.sink_bracket) && ctx.variable_sink))
    {
        return Scan::Opaque(Reason::DynamicExecution);
    }
    let mut arena: Vec<Option<Command>> = ctx.arena.into_iter().map(Some).collect();
    Scan::Scanned(commands.into_iter().map(|id| arena[id].take().expect("a command is listed once")).collect())
}

fn bash_statement(state: &mut State) {
    if let Some(structure) = state.structures.last_mut() {
        structure.count += 1;
    }
}

fn bash_in_header(state: &State) -> bool {
    matches!(state.last_structure().map(|s| s.phase), Some(Phase::Header | Phase::Pattern | Phase::Do))
}

fn at_command_start(state: &State) -> bool {
    state.words.is_empty() && !state.has_redirect && !state.compound_end && !bash_in_header(state)
}

fn pending(state: &State) -> bool {
    state.word_started || !state.words.is_empty() || state.has_redirect || state.compound_end
}

// Ends the current list; false when an operator still awaits its command.
fn end_bash_list(state: &mut State, ctx: &mut Ctx) -> bool {
    if pending(state) {
        finish_bash_command(state, ctx, false, true);
    }
    !state.dangling
}

fn close_bash_list(state: &mut State, ctx: &mut Ctx, end: isize) -> Res<(Vec<usize>, isize)> {
    if !end_bash_list(state, ctx) {
        return Err(Reason::InvalidStructure);
    }
    if let Some(reason) = state.invalid {
        return Err(reason);
    }
    if !state.structures.is_empty() {
        return Err(Reason::CompoundCommand);
    }
    Ok((std::mem::take(&mut state.commands), end))
}

fn finish_bash_word(state: &mut State, ctx: &mut Ctx) {
    if !state.word_started {
        return;
    }
    if js::includes_ascii(&state.text.word, "PS4") {
        ctx.eval_sink = true;
    }
    let evaluation = bash_word_evaluation(state, ctx);
    if ctx.dialect != Dialect::Bash
        && (js::includes_ascii(&state.text.word, "NULLCMD")
            || ((state.assignment_word || evaluation == Some(Eval::Binding)) && zsh_dynamic_name(&state.text.word)))
    {
        invalid(state, Reason::DynamicExecution);
    }
    if let Some(evaluation) = evaluation
        && bash_evaluates_expansion(&state.text, evaluation, ctx)
    {
        invalid(state, Reason::DynamicExecution);
    }
    if state.text.word != state.text.literal && state.command_word_index >= 0 {
        let builtin = word_at(&state.words, bash_builtin_index(state, ctx));
        if has_opt(&["declare", "typeset", "local"], builtin) {
            ctx.eval_sink = true;
        }
    }
    if let Some(outer) = state.outer.as_deref_mut() {
        outer.output.push(b' ' as u16);
        outer.output.extend_from_slice(&state.text.literal);
        outer.output.push(b' ' as u16);
        outer.output.extend_from_slice(&state.text.output);
    }
    let repeat_header = state.last_structure().is_some_and(|s| s.kind == Kind::Repeat && s.phase == Phase::Header);
    if !state.redirect_target && repeat_header {
        state.structures.last_mut().unwrap().phase = Phase::Do;
    }
    if !state.redirect_target && !repeat_header {
        if !state.assignment_word && state.command_word_index < 0 {
            state.command_word_index = state.words.len() as isize;
        }
        state.command_start.get_or_insert(state.word_start);
        state.words.push(state.text.word.clone());
        // Unquoted trailing continuations are ignored syntax, not part of the raw token.
        state.raw_words.push(slice(state.input, state.word_start, state.word_end).to_vec());
    }
    if state.redirect_target {
        if !state.dup_redirect || !is_fd_target(slice(state.input, state.word_start, state.word_end)) {
            state.has_file_redirect = true;
        }
        state.redirect_prefix_end.get_or_insert(state.word_end);
    }
    state.redirect_target = false;
    state.dup_redirect = false;
    state.here_string = false;
    state.text = Text::default();
    state.word_started = false;
    state.assignment_word = false;
    state.assignment_head_unsafe = false;
}

// How the shell evaluates the current word's literal text: as arithmetic, or as a value it binds to a variable
// that a later $((name)) may evaluate.
fn bash_word_evaluation(state: &State, ctx: &Ctx) -> Option<Eval> {
    if state.redirect_target {
        return if state.here_string { Some(Eval::Deferred) } else { None };
    }
    if let Some(structure) = state.last_structure() {
        if structure.kind == Kind::Repeat && structure.phase == Phase::Header {
            return Some(Eval::Arithmetic);
        }
        if structure.kind == Kind::For && structure.phase == Phase::Header && structure.saw_in {
            return Some(Eval::Deferred);
        }
    }
    if state.command_word_index < 0 {
        return if state.assignment_word { Some(Eval::Deferred) } else { None };
    }
    let name = bash_builtin_index(state, ctx);
    let command = word_at(&state.words, name)?;
    let word = state.text.word.as_slice();
    if js::eq_ascii(command, "let") || (ctx.dialect != Dialect::Bash && has(ZSH_ARITHMETIC_BUILTINS, command)) {
        return Some(Eval::Arithmetic);
    }
    if js::eq_ascii(command, "set") {
        let binding = from(&state.words, name + 1).iter().any(|w| signed_flag_has(w, b'A'));
        return Some(if binding { Eval::Binding } else { Eval::Deferred });
    }
    if has(BASH_BINDING_BUILTINS, command) {
        let options = from(&state.words, name + 1);
        if options.iter().any(|w| js::starts_with_ascii(w, "-", 0) && w.iter().any(|&c| set_has("EFin", c))) {
            return Some(Eval::Arithmetic);
        }
        // A plain `name=value` declaration without options binds a scalar unless Bash re-parses an existing array.
        if state.assignment_word
            && has(BASH_ASSIGNMENT_DECLARATIONS, command)
            && options.iter().all(|w| !js::starts_with_ascii(w, "-", 0))
            && (js::eq_ascii(command, "export")
                || js::eq_ascii(command, "readonly")
                || ctx.dialect == Dialect::Zsh
                || !assignment_reparses(word))
        {
            return Some(Eval::Deferred);
        }
        return Some(Eval::Binding);
    }
    if js::eq_ascii(command, "printf") || js::eq_ascii(command, "print") {
        let print = js::eq_ascii(command, "print");
        let flag = |w: &[u16]| if print { dash_letters_then(w, b'v') } else { starts_with_ascii(w, "-v", 0) };
        let binding = from(&state.words, name + 1).iter().any(|w| flag(w)) || flag(word);
        return if binding { Some(Eval::Binding) } else { None };
    }
    if js::eq_ascii(command, "wait") {
        let last = state.words.last().map(|w| w.as_slice()).unwrap_or(&[]);
        return if wait_flag(word, false) || wait_flag(last, true) { Some(Eval::Binding) } else { None };
    }
    if js::eq_ascii(command, "test") || js::eq_ascii(command, "[") {
        let last = state.words.last().map(|w| w.as_slice());
        if last.is_some_and(|w| js::eq_ascii(w, "-v")) {
            return Some(Eval::Binding);
        }
        if ctx.dialect != Dialect::Bash && last.is_some_and(|w| js::eq_ascii(w, "-t")) {
            return Some(Eval::Arithmetic);
        }
    }
    None
}

// Index of the word naming the builtin that runs, skipping precommand wrappers and their options.
fn bash_builtin_index(state: &State, ctx: &Ctx) -> isize {
    let words = &state.words;
    let mut name = state.command_word_index;
    while has_opt(BASH_PRECOMMANDS, word_at(words, name))
        || (ctx.dialect != Dialect::Bash && has_opt(ZSH_PRECOMMANDS, word_at(words, name)))
    {
        let precommand = word_at(words, name).unwrap();
        let dash = js::eq_ascii(precommand, "-");
        let exec = js::eq_ascii(precommand, "exec");
        name += 1;
        while !dash && word_at(words, name).is_some_and(|w| starts_with_ascii(w, "-", 0)) {
            if exec && exec_argv0_flag(word_at(words, name).unwrap()) {
                name += 1;
            }
            name += 1;
        }
    }
    name
}

fn finish_bash_command(state: &mut State, ctx: &mut Ctx, boundary: bool, newline: bool) {
    finish_bash_word(state, ctx);
    if state.redirect_target {
        invalid(state, Reason::InvalidRedirect);
    }
    state.redirect_target = false;
    if state.compound_end && !state.words.is_empty() {
        invalid(state, Reason::InvalidStructure);
    }
    let name = state.command_word_index;
    let in_header = bash_in_header(state);
    if name >= 0 && word_at(&state.words, name).is_none_or(|w| w.is_empty()) {
        invalid(state, Reason::InvalidStructure);
    }
    let mut owner = None;
    if name >= 0 && !in_header {
        let start = state.command_start.unwrap_or(0);
        let resource = js::trim_blanks(slice(state.input, start, state.word_end)).to_vec();
        let declaration = name == 0
            && !state.has_redirect
            && has_opt(BASH_DECLARATIONS, word_at(&state.raw_words, 0))
            && resource.starts_with(&state.raw_words[0]);
        let redirect_word_count = state
            .redirect_word_count
            .filter(|&count| count < state.words.len() as isize)
            .map(|count| count - name);
        let command = Command {
            resource,
            words: from(&state.words, name).to_vec(),
            raw_words: from(&state.raw_words, name).to_vec(),
            declaration,
            redirect_word_count,
            ..Command::default()
        };
        let id = push_command(ctx, command);
        state.commands.push(id);
        let builtin = bash_builtin_index(state, ctx);
        let builtin_name = word_at(&state.words, builtin);
        let builtin_args = from(&state.words, builtin + 1);
        let named = |name: &str| builtin_name.is_some_and(|w| js::eq_ascii(w, name));
        if named("eval") {
            ctx.eval_sink = true;
        }
        if named("set") && builtin_args.iter().any(|w| dash_alnum_has(w, b'k') || js::eq_ascii(w, "keyword")) {
            state.invalid.get_or_insert(Reason::DynamicExecution);
        }
        // Dash expands an alias defined earlier in the same script, which can name any command.
        if named("alias") && builtin_args.iter().any(|w| js::includes(w, b'=' as u16)) {
            state.invalid.get_or_insert(Reason::DynamicExecution);
        }
        // Zsh options such as globsubst, kshglob, and promptsubst enable evaluation on expansions and print -P.
        if ctx.dialect != Dialect::Bash
            && (named("emulate")
                || ((named("setopt")
                    || named("unsetopt")
                    || (named("set") && builtin_args.iter().any(|w| signed_flag_has(w, b'o'))))
                    && builtin_args.iter().any(|w| dash_alnum_has(w, b'm') || zsh_evaluating_option(w))))
        {
            state.invalid.get_or_insert(Reason::DynamicExecution);
        }
        // Bash compgen -C and -F execute their argument immediately to generate completions.
        if ctx.dialect != Dialect::Zsh
            && named("compgen")
            && builtin_args.iter().any(|w| starts_with_ascii(w, "-", 0) && w.iter().any(|&c| set_has("CF", c)))
        {
            state.invalid.get_or_insert(Reason::DynamicExecution);
        }
        if has_opt(BASH_BINDING_BUILTINS, builtin_name) {
            for heredoc in &mut ctx.heredocs[state.heredocs] {
                heredoc.binding = true;
            }
        }
        owner = Some(id);
    } else if name < 0
        && !in_header
        && let Some(command_start) = state.command_start
        && (state.has_file_redirect || (state.has_redirect && !state.compound_end))
    {
        let raw_span = slice(state.input, command_start, state.word_end);
        let leading = raw_span.iter().take_while(|&&c| matches!(c, 0x20 | 0x09 | 0x0a)).count() as isize;
        let rest = js::slice_from(raw_span, leading);
        let trailing = rest.iter().rev().take_while(|&&c| matches!(c, 0x20 | 0x09 | 0x0a)).count();
        let resource = rest[..rest.len() - trailing].to_vec();
        if !resource.is_empty() {
            let prefix_end = match state.redirect_prefix_end {
                Some(end) => 1.max(len(&resource).min(end - command_start - leading)),
                None => len(&resource),
            };
            let prefix_token = slice(&resource, 0, prefix_end).to_vec();
            let command = Command {
                resource,
                words: vec![prefix_token.clone()],
                raw_words: vec![prefix_token],
                redirect_prefix_end: Some(prefix_end),
                ..Command::default()
            };
            let id = push_command(ctx, command);
            state.commands.push(id);
            owner = Some(id);
        }
    }
    let command_start = state.command_start;
    for heredoc in &mut ctx.heredocs[state.heredocs] {
        if let Some(owner) = owner
            && newline
            && !heredoc.owned
        {
            heredoc.command = Some(owner);
            heredoc.start = command_start;
        }
        heredoc.owned = true;
    }
    if boundary && state.words.is_empty() && !state.has_redirect && !state.compound_end && !in_header {
        invalid(state, Reason::InvalidStructure);
    }
    if state.ampersand_redirect_words.is_some_and(|limit| state.words.len() as isize > limit) {
        invalid(state, Reason::InvalidRedirect);
    }
    let nested = std::mem::take(&mut state.nested_commands);
    state.commands.extend(nested);
    if !in_header && (!state.words.is_empty() || state.has_redirect) {
        bash_statement(state);
    }
    state.words.clear();
    state.raw_words.clear();
    state.command_word_index = -1;
    state.command_start = None;
    state.dup_redirect = false;
    state.has_redirect = false;
    state.has_file_redirect = false;
    state.redirect_word_count = None;
    state.redirect_prefix_end = None;
    state.ampersand_redirect_words = None;
    state.compound_end = false;
    state.dangling = false;
}

fn scan_bash(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    close: Option<Close>,
    // A group shares its parent's heredocs; a substitution starts its own.
    heredocs: Option<usize>,
    outer: Option<&mut Text>,
) -> Res<(Vec<usize>, isize)> {
    if depth > MAX_SUBSTITUTION_DEPTH || ctx.remaining < 0 {
        return Err(Reason::InvalidStructure);
    }
    let heredoc_list = heredocs.unwrap_or_else(|| {
        ctx.heredocs.push(Vec::new());
        ctx.heredocs.len() - 1
    });
    let mut state = State {
        input,
        commands: Vec::new(),
        nested_commands: Vec::new(),
        words: Vec::new(),
        raw_words: Vec::new(),
        heredocs: heredoc_list,
        structures: Vec::new(),
        text: Text::default(),
        word_started: false,
        word_start: start,
        word_end: start,
        command_start: None,
        redirect_word_count: None,
        redirect_prefix_end: None,
        ampersand_redirect_words: None,
        command_word_index: -1,
        assignment_word: false,
        assignment_head_unsafe: false,
        invalid: None,
        redirect_target: false,
        dup_redirect: false,
        here_string: false,
        has_redirect: false,
        has_file_redirect: false,
        compound_end: false,
        dangling: false,
        outer,
    };
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        // Line continuations are removed before tokens are read.
        if c == b'\\' as u16 && js::is(at(input, index + 1), b'\n') {
            index += 1;
            continue;
        }
        if !state.word_started {
            state.word_start = index;
            if c == b' ' as u16 || c == b'\t' as u16 {
                continue;
            }
            if c == b'}' as u16
                && state.words.is_empty()
                && !state.redirect_target
                && matches!(close, Some(Close::Brace | Close::Nofork))
                && state.structures.is_empty()
                && (heredocs.is_some() || ctx.heredocs[state.heredocs].is_empty())
                && (close == Some(Close::Nofork) || brace_close_ahead(input, index + 1))
            {
                return close_bash_list(&mut state, ctx, index);
            }
            if c == b'}' as u16 {
                return Err(if state.structures.is_empty() { Reason::InvalidStructure } else { Reason::CompoundCommand });
            }
            if let Some(step) = scan_bash_command_start(&mut state, index, depth, ctx)? {
                index = step;
                continue;
            }
        }
        if c == b'\\' as u16 && index + 1 >= n {
            return Err(Reason::UnterminatedEscape);
        }
        if !state.assignment_word
            && (set_has("'\"\\", c)
                || (c == b'$' as u16 && (js::is(at(input, index + 1), b'\'') || js::is(at(input, index + 1), b'"'))))
        {
            state.assignment_head_unsafe = true;
        }
        let in_case_pattern = state.last_structure().is_some_and(|s| s.phase == Phase::Pattern);
        // Elsewhere a parenthesized group inside a word is a Zsh glob qualifier, which can run code.
        let word = &state.text.word;
        let array = (c == b'=' as u16 || c == b'(' as u16)
            && state.assignment_word
            && word.len() > 1
            && word.last() == Some(&(b'=' as u16))
            && !word.ends_with(&[b'=' as u16, b'=' as u16])
            && (state.command_word_index < 0
                || has_opt(BASH_ASSIGNMENT_DECLARATIONS, word_at(&state.raw_words, state.command_word_index)));
        if c == b'=' as u16 && (!state.word_started || array) && js::is(at(input, index + 1), b'(') {
            let end = scan_bash_nested(input, index + 2, depth, ctx, &mut state.nested_commands, Close::Paren, None)?;
            state.word_started = true;
            state.text.word.extend_from_slice(slice(input, index, end + 1));
            index = end;
            state.word_end = index + 1;
            continue;
        }
        let unit = if set_has(BASH_UNIT_STARTS, c) {
            let allow_bracket = !state.assignment_word
                && state.command_word_index >= 0
                && !has_opt(BASH_DECLARATIONS, word_at(&state.raw_words, 0));
            scan_bash_unit(
                input,
                index,
                depth,
                ctx,
                &mut state.nested_commands,
                Mode::Word,
                allow_bracket,
                Some(&mut state.text),
            )?
        } else {
            None
        };
        if let Some(unit) = unit {
            state.word_started = true;
            index = unit;
            state.word_end = index + 1;
            continue;
        }
        if c == b'[' as u16
            && !in_case_pattern
            && !state.assignment_word
            && !state.assignment_head_unsafe
            && state.command_word_index < 0
            && is_name(&state.text.word)
        {
            let mut subscript = Vec::new();
            let end = scan_bash_subscript(
                input,
                index + 1,
                depth,
                ctx,
                &mut subscript,
                &BASH_SPANS_ASSIGNMENT_SUBSCRIPT,
                &mut Text::default(),
            )?;
            let assign = bash_operator(input, end + 1, &["+=", "="])?;
            if assign.is_some() {
                // Dash has no arrays and word-splits unquoted expansions inside `name[subscript]=value`.
                if ctx.dialect == Dialect::Posix && slice(input, index + 1, end).iter().any(|&c| set_has("$`", c)) {
                    return Err(Reason::InvalidStructure);
                }
                state.nested_commands.extend(subscript);
                state.assignment_word = true;
                state.text.word.extend_from_slice(slice(input, index, end + 1));
                index = end;
                state.word_end = index + 1;
                continue;
            }
        }
        if c == b'(' as u16
            && (array
                || ((in_case_pattern || ctx.dialect == Dialect::Bash)
                    && state.text.word.last().is_some_and(|&c| set_has("?*+@!", c))))
        {
            let mode = if array { ArrayMode::Array } else { ArrayMode::Pattern };
            let end = scan_bash_array_or_pattern(input, index, depth + 1, ctx, &mut state.nested_commands, mode)?;
            if bash_crosses_heredoc(&state, ctx, index, end) {
                return Err(Reason::Heredoc);
            }
            // Zsh ends an array assignment at its closing parenthesis, so text right after it starts a command.
            if array && end + 1 < n && !set_has(" \t\n;&|<>)", input[(end + 1) as usize]) {
                return Err(Reason::InvalidStructure);
            }
            state.word_started = true;
            state.text.word.extend_from_slice(slice(input, index, end + 1));
            index = end;
            state.word_end = index + 1;
            continue;
        }
        if c == b'#' as u16 && !state.word_started {
            let newline = js::index_of(input, b'\n' as u16, index);
            if newline == -1 {
                break;
            }
            index = newline - 1;
            continue;
        }
        if set_has("<>&|;\n", c) {
            index = scan_bash_operator_or_separator(&mut state, index, depth, ctx)?;
            continue;
        }
        if in_case_pattern && c == b')' as u16 {
            finish_bash_word(&mut state, ctx);
            index -= 1;
            continue;
        }
        if c == b')' as u16 {
            if close != Some(Close::Paren)
                || !state.structures.is_empty()
                || (heredocs.is_none() && !ctx.heredocs[state.heredocs].is_empty())
            {
                return Err(Reason::CompoundCommand);
            }
            return close_bash_list(&mut state, ctx, index);
        }
        if c == b'(' as u16
            && ctx.dialect == Dialect::Zsh
            && state.word_started
            && !in_case_pattern
            && let Some(qualifier_end) = zsh_glob_qualifier(input, index)
        {
            let end = qualifier_end - 1;
            bash_append(&mut state.text, slice(input, index, end + 1));
            index = end;
            state.word_end = index + 1;
            continue;
        }
        if c == b'(' as u16 {
            return Err(if state.word_started { Reason::DynamicExecution } else { Reason::CompoundCommand });
        }
        if js::is_space(c) && !set_has(" \t\n", c) {
            return Err(Reason::InvalidStructure);
        }
        if c == b' ' as u16 || c == b'\t' as u16 {
            finish_bash_word(&mut state, ctx);
            continue;
        }
        state.word_started = true;
        if c == b'=' as u16
            && !state.assignment_word
            && !state.assignment_head_unsafe
            && is_assignment_head(&state.text.word)
        {
            state.assignment_word = true;
        }
        bash_append(&mut state.text, &[c]);
        state.word_end = index + 1;
    }

    if let Some(close) = close {
        return Err(if close == Close::Paren { Reason::CommandSubstitution } else { Reason::InvalidStructure });
    }
    if !ctx.heredocs[state.heredocs].is_empty() {
        return Err(Reason::Heredoc);
    }
    close_bash_list(&mut state, ctx, n)
}

// Steps return the last consumed index.
fn scan_bash_operator_or_separator(state: &mut State, index: isize, depth: isize, ctx: &mut Ctx) -> Res<isize> {
    let input = state.input;
    let c = input[index as usize];
    let redirect = if set_has("<>&", c) { bash_operator(input, index, BASH_REDIRECTS)? } else { None };
    let case_head = state.last_structure().map(|s| (s.kind, s.phase));
    if redirect.is_some() && matches!(case_head, Some((Kind::Case, phase)) if phase != Phase::Body) {
        return Err(Reason::InvalidRedirect);
    }
    if let Some(redirect) = redirect {
        let length = redirect.len() as isize;
        state.has_redirect = true;
        state.command_start.get_or_insert(state.word_start);
        if state.redirect_target {
            invalid(state, Reason::InvalidRedirect);
        }
        let fd_prefix = state.word_started && !state.assignment_head_unsafe && is_fd_prefix(&state.text.word);
        if fd_prefix {
            state.text = Text::default();
            state.word_started = false;
        }
        if !fd_prefix {
            finish_bash_word(state, ctx);
        }
        if redirect.starts_with('&') && ctx.dialect == Dialect::Posix {
            state.ampersand_redirect_words.get_or_insert(state.words.len() as isize);
        }
        if state.redirect_word_count.is_none() && state.command_word_index >= 0 {
            state.redirect_word_count = Some(state.words.len() as isize);
        }
        if redirect == "<<" || redirect == "<<-" {
            let Some(delimiter) = bash_heredoc_delimiter(input, index + length, redirect == "<<-") else {
                return Err(Reason::InvalidRedirect);
            };
            state.has_file_redirect = true;
            let prefix_end = if set_has_or_empty(" \t", at(input, index + length)) {
                index + length
            } else {
                delimiter.end + 1
            };
            state.redirect_prefix_end.get_or_insert(prefix_end);
            let end = delimiter.end;
            ctx.heredocs[state.heredocs].push(Heredoc {
                delimiter: delimiter.delimiter,
                dash_delimiter: delimiter.dash_delimiter,
                quoted: delimiter.quoted,
                tabs: delimiter.tabs,
                binding: false,
                owned: false,
                command: None,
                start: None,
            });
            state.word_end = end + 1;
            state.redirect_target = false;
            state.dup_redirect = false;
            return Ok(end);
        }
        if set_has_or_empty(" \t\n", at(input, index + length)) {
            state.redirect_prefix_end.get_or_insert(index + length);
        }
        state.redirect_target = true;
        state.dup_redirect = redirect == "<&" || redirect == ">&";
        state.here_string = redirect == "<<<";
        return Ok(index + length - 1);
    }
    if state.last_structure().is_some_and(|s| s.phase == Phase::Pattern) && c == b'|' as u16 {
        finish_bash_word(state, ctx);
        return Ok(index - 1);
    }
    let structure = state.last_structure().map(|s| (s.kind, s.phase, s.saw_in));
    let case_body = matches!(structure, Some((Kind::Case, Phase::Body, _)));
    let operators = match (ctx.dialect == Dialect::Zsh, case_body) {
        (true, true) => ZSH_CASE_OPERATORS,
        (true, false) => ZSH_LIST_OPERATORS,
        (false, true) => BASH_CASE_OPERATORS,
        (false, false) => BASH_LIST_OPERATORS,
    };
    let operator = bash_operator(input, index, operators)?;
    let separator: JsString = operator.map(units).unwrap_or_else(|| vec![c]);
    let sep = |text: &str| js::eq_ascii(&separator, text);
    let seplen = len(&separator);
    if case_body && separator[0] == b';' as u16 && separator.len() > 1 {
        if !end_bash_list(state, ctx) {
            return Err(Reason::InvalidStructure);
        }
        let structure = state.structures.last_mut().unwrap();
        structure.phase = Phase::Pattern;
        structure.pattern_started = false;
        return Ok(index + seplen - 1);
    }
    if let Some((Kind::Case, phase @ (Phase::Header | Phase::Pattern), _)) = structure {
        if !sep("\n") {
            return Err(Reason::CompoundCommand);
        }
        if phase == Phase::Pattern && (state.word_started || !state.words.is_empty()) {
            return Err(Reason::CompoundCommand);
        }
        finish_bash_word(state, ctx);
        return read_bash_heredocs(state, index, depth, ctx);
    }
    if let Some((Kind::For | Kind::Repeat, phase @ (Phase::Header | Phase::Do), saw_in)) = structure {
        if !sep(";") && !sep("\n") {
            return Err(Reason::CompoundCommand);
        }
        if phase == Phase::Header && !saw_in && !state.word_started && state.words.is_empty() {
            return Err(Reason::CompoundCommand);
        }
    }
    if sep("\n") && !pending(state) {
        return read_bash_heredocs(state, index, depth, ctx);
    }
    if sep(";") && ctx.dialect == Dialect::Zsh && !pending(state) && !state.dangling {
        return Ok(index);
    }
    finish_bash_command(state, ctx, true, sep("\n"));
    if let Some((Kind::For, Phase::Header, _)) = structure {
        state.structures.last_mut().unwrap().phase = Phase::Do;
    }
    state.dangling = !(sep(";") || sep("\n") || (separator[0] == b'&' as u16 && !sep("&&")));
    // Reprocess the newline to read pending heredoc bodies.
    if sep("\n") && !ctx.heredocs[state.heredocs].is_empty() {
        return Ok(index - 1);
    }
    Ok(index + seplen - 1)
}

// Reads the pending heredoc bodies after the newline at index and returns the last index they cover.
fn read_bash_heredocs(state: &mut State, index: isize, depth: isize, ctx: &mut Ctx) -> Res<isize> {
    let mut end = index;
    let heredocs = std::mem::take(&mut ctx.heredocs[state.heredocs]);
    for heredoc in heredocs {
        let Some(body) = bash_heredoc(state.input, end + 1, &heredoc) else {
            return Err(Reason::Heredoc);
        };
        if let Some(command) = heredoc.command {
            ctx.arena[command].resource = js::trim(slice(state.input, heredoc.start.unwrap_or(0), body.end)).to_vec();
        }
        let mut text = if heredoc.quoted {
            Text { word: body.source.clone(), literal: body.source.clone(), output: JsString::new() }
        } else {
            Text::default()
        };
        if !heredoc.quoted {
            scan_bash_span(&body.source, 0, depth, ctx, &mut state.commands, &BASH_SPANS_HEREDOC, true, Some(&mut text))?;
        }
        if heredoc.binding {
            bash_evaluates_expansion(&text, Eval::Deferred, ctx);
        }
        end = body.end;
    }
    Ok(end)
}

// Whether a construct read as one unit holds a newline token that would start pending heredoc bodies.
fn bash_crosses_heredoc(state: &State, ctx: &Ctx, start: isize, end: isize) -> bool {
    !ctx.heredocs[state.heredocs].is_empty() && js::includes(slice(state.input, start, end), b'\n' as u16)
}

fn scan_bash_command_start(state: &mut State, index: isize, depth: isize, ctx: &mut Ctx) -> Res<Option<isize>> {
    let input = state.input;
    let c = input[index as usize];
    // TS holds the structure object itself, which a pop below leaves stale: keep what it was.
    let structure = state.last_structure().map(|s| (s.kind, s.phase, s.pattern_started));
    let kind = structure.map(|s| s.0);
    let phase = structure.map(|s| s.1);
    if phase == Some(Phase::Pattern) {
        let pattern_started = structure.unwrap().2;
        if !pattern_started && state.words.is_empty() && c == b'(' as u16 {
            state.structures.last_mut().unwrap().pattern_started = true;
            return Ok(Some(index));
        }
        if c == b'|' as u16 {
            if state.words.is_empty() {
                return Err(Reason::CompoundCommand);
            }
            finish_bash_word(state, ctx);
            state.structures.last_mut().unwrap().pattern_started = true;
            return Ok(Some(index));
        }
        if c == b')' as u16 {
            if state.words.is_empty() {
                return Err(Reason::CompoundCommand);
            }
            finish_bash_command(state, ctx, false, false);
            let structure = state.structures.last_mut().unwrap();
            structure.phase = Phase::Body;
            structure.count = 0;
            return Ok(Some(index));
        }
    }
    if kind == Some(Kind::For) && phase == Some(Phase::Header) && c == b'(' as u16 && !js::is(at(input, index + 1), b'(')
    {
        let end = scan_bash_array_or_pattern(input, index, depth + 1, ctx, &mut state.nested_commands, ArrayMode::Array)?;
        if bash_crosses_heredoc(state, ctx, index, end) {
            return Err(Reason::Heredoc);
        }
        finish_bash_command(state, ctx, false, false);
        // Zsh permits a sublist or brace group directly after the value list, without do/done.
        let structure = state.structures.last_mut().unwrap();
        structure.phase = Phase::Do;
        structure.parenthesized = true;
        if !do_ahead(input, end + 1) {
            state.structures.pop();
        }
        return Ok(Some(end));
    }
    if kind == Some(Kind::Repeat)
        && phase == Some(Phase::Do)
        && c != b'\n' as u16
        && c != b'#' as u16
        && c != b';' as u16
        && !token_at(input, index).is_some_and(|t| js::eq_ascii(t, "do"))
    {
        state.structures.pop();
    }
    if let Some(step) = scan_bash_keyword(state, index, depth, ctx)? {
        return Ok(Some(step));
    }
    if kind == Some(Kind::For) && phase == Some(Phase::Do) && c != b'\n' as u16 && c != b'#' as u16 && c != b';' as u16
    {
        if c != b'{' as u16 || !space_continuation_ahead(input, index + 1) {
            return Err(Reason::CompoundCommand);
        }
        state.structures.pop();
    }
    let at_start = at_command_start(state);
    let function_head = if at_start && c == b'(' as u16 { bash_function_head_length(input, index) } else { 0 };
    if function_head > 0 {
        return Ok(Some(index + function_head - 1));
    }
    if at_start && c == b'!' as u16 && negation_ahead(input, index + 1) {
        return Ok(Some(index));
    }
    let for_header = kind == Some(Kind::For) && phase == Some(Phase::Header);
    let compound_open =
        if (at_start || for_header) && set_has("([", c) { bash_operator(input, index, &["((", "[["])? } else { None };
    if compound_open == Some("((") {
        if for_header && !state.words.is_empty() {
            return Err(Reason::CompoundCommand);
        }
        let span = if for_header { &BASH_SPANS_FOR_ARITHMETIC } else { &BASH_SPANS_ARITHMETIC };
        match scan_bash_arithmetic(input, index + 2, depth, ctx, &mut state.commands, span)? {
            Some(end) => {
                if bash_crosses_heredoc(state, ctx, index, end) {
                    return Err(Reason::Heredoc);
                }
                if !for_header && ctx.dialect == Dialect::Posix {
                    // Dash reads `((` as two nested subshells, so posix reports both readings.
                    let (subshells, subshells_end) =
                        scan_bash(input, index + 1, depth + 1, ctx, Some(Close::Paren), None, None)?;
                    if subshells_end != end {
                        return Err(Reason::CompoundCommand);
                    }
                    let seen: Vec<(JsString, Vec<JsString>)> = state
                        .commands
                        .iter()
                        .map(|&id| (ctx.arena[id].resource.clone(), ctx.arena[id].words.clone()))
                        .collect();
                    for id in subshells {
                        let command = &ctx.arena[id];
                        if !seen.iter().any(|(resource, words)| *resource == command.resource && *words == command.words)
                        {
                            state.commands.push(id);
                        }
                    }
                }
                if for_header {
                    let structure = state.structures.last_mut().unwrap();
                    structure.phase = Phase::Do;
                    structure.saw_in = true;
                }
                if !for_header {
                    bash_statement(state);
                    state.compound_end = true;
                }
                return Ok(Some(end));
            }
            None => {
                if for_header {
                    return Err(Reason::InvalidStructure);
                }
            }
        }
    }
    if at_start && (c == b'(' as u16 || (c == b'{' as u16 && space_continuation_ahead(input, index + 1))) {
        let close = if c == b'{' as u16 { Close::Brace } else { Close::Paren };
        let (commands, end) = scan_bash(input, index + 1, depth + 1, ctx, Some(close), Some(state.heredocs), None)?;
        if js::trim(slice(input, index + 1, end)).is_empty() {
            return Err(Reason::InvalidStructure);
        }
        state.commands.extend(commands);
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    if at_start && compound_open == Some("[[") && space_continuation_ahead(input, index + 2) {
        let end = scan_bash_conditional(input, index + 2, depth, ctx, &mut state.commands)?;
        if bash_crosses_heredoc(state, ctx, index, end) {
            return Err(Reason::Heredoc);
        }
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    Ok(None)
}

fn scan_bash_keyword(state: &mut State, index: isize, depth: isize, ctx: &mut Ctx) -> Res<Option<isize>> {
    let input = state.input;
    let c = input[index as usize];
    if !js::is_ident_start(c) {
        return Ok(None);
    }
    let token = token_at(input, index);
    let is_token = |word: &str| token.is_some_and(|t| js::eq_ascii(t, word));
    let end = index + token.map_or(0, len) - 1;
    let structure = state.last_structure().map(|s| (s.kind, s.phase, s.saw_in, s.parenthesized, s.pattern_started));
    if let Some((Kind::Case, Phase::Header, ..)) = structure
        && is_token("in")
    {
        if state.words.len() != 1 || state.has_redirect {
            return Err(Reason::CompoundCommand);
        }
        finish_bash_command(state, ctx, false, false);
        let structure = state.structures.last_mut().unwrap();
        structure.phase = Phase::Pattern;
        structure.pattern_started = false;
        return Ok(Some(end));
    }
    if let Some((Kind::For, phase @ (Phase::Header | Phase::Do), false, false, _)) = structure
        && is_token("in")
        && (phase == Phase::Do || !state.words.is_empty())
    {
        let structure = state.structures.last_mut().unwrap();
        structure.saw_in = true;
        structure.phase = Phase::Header;
        return Ok(Some(end));
    }
    if let Some((_, Phase::Pattern, _, _, false)) = structure
        && state.words.is_empty()
        && is_token("esac")
    {
        state.structures.pop();
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    // POSIX `for name do` omits the in list.
    if let Some((Kind::For, Phase::Header, false, ..)) = structure
        && state.words.len() == 1
        && !state.has_redirect
        && is_token("do")
    {
        finish_bash_command(state, ctx, false, false);
        state.structures.last_mut().unwrap().phase = Phase::Do;
    }
    let in_header = bash_in_header(state);
    let phase_now = state.last_structure().map(|s| s.phase);
    if let Some(token) = token
        && ["then", "elif", "else", "fi", "do", "done", "esac"].iter().any(|k| js::eq_ascii(token, k))
        && state.words.is_empty()
        && !state.redirect_target
        && (!in_header || (is_token("do") && phase_now == Some(Phase::Do)))
    {
        // Bash and Dash read a reserved word after a redirect as a command word, while Zsh reads a keyword.
        if (state.has_redirect && ctx.dialect != Dialect::Zsh) || !end_bash_list(state, ctx) || structure.is_none() {
            return Err(Reason::CompoundCommand);
        }
        let s = state.structures.last_mut().unwrap();
        let looped = matches!(s.kind, Kind::For | Kind::While | Kind::Until | Kind::Repeat);
        if is_token("then") {
            if s.kind != Kind::If || s.phase != Phase::Condition || s.count == 0 {
                return Err(Reason::CompoundCommand);
            }
            s.phase = Phase::Body;
            s.count = 0;
            return Ok(Some(end));
        }
        if is_token("elif") || is_token("else") {
            if s.kind != Kind::If || s.phase != Phase::Body || s.count == 0 || s.saw_else {
                return Err(Reason::CompoundCommand);
            }
            s.phase = if is_token("elif") { Phase::Condition } else { Phase::Body };
            s.saw_else = is_token("else");
            s.count = 0;
            return Ok(Some(end));
        }
        if is_token("do") {
            if !looped
                || !matches!(s.phase, Phase::Condition | Phase::Do)
                || (s.kind != Kind::For && s.kind != Kind::Repeat && s.count == 0)
            {
                return Err(Reason::CompoundCommand);
            }
            s.phase = Phase::Body;
            s.count = 0;
            return Ok(Some(end));
        }
        if (is_token("fi") && (s.kind != Kind::If || s.phase != Phase::Body || s.count == 0))
            || (is_token("done") && (!looped || s.phase != Phase::Body || s.count == 0))
            || (is_token("esac") && (s.kind != Kind::Case || s.phase == Phase::Header))
        {
            return Err(Reason::CompoundCommand);
        }
        state.structures.pop();
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    if !at_command_start(state) {
        return Ok(None);
    }
    let definition_length = bash_function_head_length(input, index);
    if definition_length > 0 {
        return Ok(Some(index + definition_length - 1));
    }
    if let Some(token) = token
        && let Some(kind) = compound_kind(token, ctx.dialect)
    {
        if depth + state.structures.len() as isize >= MAX_SUBSTITUTION_DEPTH {
            return Err(Reason::CompoundCommand);
        }
        let header = ["for", "select", "case", "repeat"].iter().any(|k| js::eq_ascii(token, k));
        state.structures.push(Structure {
            kind,
            phase: if header { Phase::Header } else { Phase::Condition },
            count: 0,
            saw_else: false,
            saw_in: false,
            pattern_started: false,
            parenthesized: false,
        });
        // The compound command itself satisfies a preceding list operator.
        state.dangling = false;
        return Ok(Some(end));
    }
    let prefix = if is_token("coproc") {
        coproc_ahead(input, index)
    } else if is_token("time") {
        time_ahead(input, index)
    } else {
        None
    };
    if let Some(length) = prefix
        && length > 0
    {
        return Ok(Some(index + length - 1));
    }
    Ok(None)
}

fn compound_kind(token: &[u16], dialect: Dialect) -> Option<Kind> {
    if js::eq_ascii(token, "if") {
        Some(Kind::If)
    } else if js::eq_ascii(token, "while") {
        Some(Kind::While)
    } else if js::eq_ascii(token, "until") {
        Some(Kind::Until)
    } else if js::eq_ascii(token, "for") || js::eq_ascii(token, "select") {
        Some(Kind::For)
    } else if js::eq_ascii(token, "case") {
        Some(Kind::Case)
    } else if js::eq_ascii(token, "repeat") && dialect != Dialect::Bash {
        Some(Kind::Repeat)
    } else {
        None
    }
}

fn bash_function_head_length(input: &[u16], start: isize) -> isize {
    let Some(head) = function_head(input, start) else {
        return 0;
    };
    let rejected = if head.function {
        head.name.is_none()
    } else {
        !head.parens || head.name.is_some_and(|name| has(BASH_NON_FUNCTION_KEYWORDS, slice(input, name.0, name.1)))
    };
    if rejected {
        return 0;
    }
    // Blanks, comments, and newlines before the body stay for the caller, where newlines start heredoc bodies.
    if function_body(input, start + head.length) { head.length - head.trailing } else { 0 }
}

// Word text is unquoted. Quoted text follows double-quote rules. Arithmetic text, including subscripts,
// expands like double-quoted text while its matcher still lets a backslash escape any character.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Word,
    Quoted,
    Arithmetic,
}

struct Span {
    open: Option<u16>,
    close: Option<u16>,
    reject: &'static str,
    mode: Mode,
}

const fn span(open: Option<u8>, close: Option<u8>, reject: &'static str, mode: Mode) -> Span {
    Span {
        open: match open {
            Some(c) => Some(c as u16),
            None => None,
        },
        close: match close {
            Some(c) => Some(c as u16),
            None => None,
        },
        reject,
        mode,
    }
}

const BASH_SPANS_DOUBLE: Span = span(None, Some(b'"'), "", Mode::Quoted);
const BASH_SPANS_HEREDOC: Span = span(None, None, "", Mode::Quoted);
// Bash rereads `((...))` it cannot parse as arithmetic, such as text with a comment, as nested subshells.
const BASH_SPANS_ARITHMETIC: Span = span(Some(b'('), Some(b')'), ";#}", Mode::Arithmetic);
const BASH_SPANS_FOR_ARITHMETIC: Span = span(Some(b'('), Some(b')'), "#}", Mode::Arithmetic);
const BASH_SPANS_BRACKET_ARITHMETIC: Span = span(None, Some(b']'), ";|&<>()[\n'\"\\#", Mode::Arithmetic);
const BASH_SPANS_SUBSCRIPT: Span = span(Some(b'['), Some(b']'), "\"}", Mode::Arithmetic);
const BASH_SPANS_QUOTED_SUBSCRIPT: Span = span(Some(b'['), Some(b']'), "\"", Mode::Quoted);
// Dash splits an assignment subscript at blanks and operators, so Bash and Zsh assignments diverge.
const BASH_SPANS_ASSIGNMENT_SUBSCRIPT: Span =
    span(Some(b'['), Some(b']'), " \t\n\r\x0b\x0c;&|<>()", Mode::Arithmetic);
// Single quotes are literal here in some shells and quoting in others, so reject what either reading
// would parse structurally and scan the contents for expansions. Inside a double-quoted parameter, Zsh and
// Dash read them literally even within subscripts and arithmetic, where `"` and `}` would end the parameter.
const BASH_SPANS_ARITHMETIC_QUOTE: Span = span(None, Some(b'\''), "()[];\"}", Mode::Quoted);
const BASH_SPANS_ARITHMETIC_DOUBLE: Span = span(None, Some(b'"'), "()[];}", Mode::Quoted);
const BASH_SPANS_PARAMETER_QUOTE: Span = span(None, Some(b'\''), "\"}[]", Mode::Quoted);

// Scans one quoting or expansion unit at index into commands, appending its text when given a sink.
// Returns the unit's last index, or None when the character is ordinary text.
#[allow(clippy::too_many_arguments)]
fn scan_bash_unit(
    input: &[u16],
    index: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    mode: Mode,
    allow_bracket: bool,
    mut text: Option<&mut Text>,
) -> Res<Option<isize>> {
    let c = input[index as usize];
    let next = at(input, index + 1).unwrap_or(0);
    if c == b'\\' as u16 {
        if mode == Mode::Quoted && !set_has("$`\"\\\n", next) {
            return Ok(None);
        }
        if let Some(text) = text.as_deref_mut()
            && next != b'\n' as u16
        {
            bash_append(text, slice(input, index + 1, index + 2));
        }
        return Ok(Some(index + 1));
    }
    if c == b'$' as u16 && ctx.dialect != Dialect::Bash && zsh_evaluates(input, index) {
        return Err(Reason::DynamicExecution);
    }
    if c == b'$' as u16
        && ctx.dialect != Dialect::Bash
        && (mode == Mode::Word || mode == Mode::Quoted)
        && let Some(bracket) = zsh_subscript(input, index)
    {
        let mut subscript = Text::default();
        let span = if mode == Mode::Quoted { &BASH_SPANS_QUOTED_SUBSCRIPT } else { &BASH_SPANS_SUBSCRIPT };
        let end = scan_bash_subscript(input, bracket, depth, ctx, commands, span, &mut subscript)?;
        if let Some(text) = text.as_deref_mut() {
            text.word.extend_from_slice(slice(input, index, bracket));
            text.word.extend_from_slice(&subscript.word);
            text.word.push(b']' as u16);
        }
        return Ok(Some(end));
    }
    let opener = if c == b'$' as u16 {
        bash_operator(input, index, &["$$", "$((", "$(", "${", "$[", "$'", "$\""])?
    } else if mode == Mode::Word && c == b'<' as u16 {
        bash_operator(input, index, &["<("])?
    } else if mode == Mode::Word && c == b'>' as u16 {
        bash_operator(input, index, &[">("])?
    } else {
        None
    };
    if opener == Some("$$") {
        let quote_start = line_continuations(input, index + 2).unwrap_or(index + 2);
        if js::is(at(input, quote_start), b'\'') {
            let next_quote = js::index_of(input, b'\'' as u16, quote_start + 1);
            if next_quote < 0 {
                return Err(Reason::UnterminatedQuote);
            }
            // Zsh and Bash 3.2 inside ${...} can parse $$'...' with ANSI-C escapes while Bash 5/Dash treat $$ as PID.
            if js::includes(slice(input, quote_start + 1, next_quote), b'\\' as u16)
                && js::index_of(input, b'\'' as u16, next_quote + 1) >= 0
            {
                return Err(Reason::UnterminatedQuote);
            }
        }
        if let Some(text) = text.as_deref_mut() {
            text.word.extend_from_slice(&units("$$"));
        }
        return Ok(Some(index + 1));
    }
    if mode == Mode::Word && opener == Some("$'") {
        let Some(quote) = bash_ansi_quote(input, index + 1, true) else {
            return Err(Reason::UnterminatedQuote);
        };
        let first_quote = js::index_of(input, b'\'' as u16, index + 2);
        // Dash does not support $'...' and closes the single-quoted span at the first `'`, even after `\`.
        if quote.end != first_quote
            && ((ctx.dialect != Dialect::Bash && index >= 2 && js::is(at(input, index - 2), b'$'))
                || (ctx.dialect == Dialect::Posix
                    && (js::index_of(input, b'\'' as u16, first_quote + 1) < quote.end
                        || dash_unsafe_tail(js::slice_from(input, first_quote + 1)))))
        {
            return Err(Reason::UnterminatedQuote);
        }
        if let Some(text) = text.as_deref_mut() {
            bash_append(text, &quote.value);
        }
        return Ok(Some(quote.end));
    }
    if mode == Mode::Arithmetic && opener == Some("$'") {
        let quote = bash_ansi_quote(input, index + 1, true);
        let Some(quote) = quote.filter(|quote| {
            quote.end == js::index_of(input, b'\'' as u16, index + 2)
                && !quote.value.iter().any(|&c| set_has("()[];\"}", c))
        }) else {
            return Err(Reason::InvalidStructure);
        };
        if let Some(text) = text.as_deref_mut() {
            bash_append(text, &quote.value);
        }
        return scan_bash_span(input, index + 2, depth, ctx, commands, &BASH_SPANS_ARITHMETIC_QUOTE, allow_bracket, text)
            .map(Some);
    }
    if mode == Mode::Word && c == b'\'' as u16 {
        let end = js::index_of(input, b'\'' as u16, index + 1);
        if end < 0 {
            return Err(Reason::UnterminatedQuote);
        }
        if let Some(text) = text.as_deref_mut() {
            bash_append(text, slice(input, index + 1, end));
        }
        return Ok(Some(end));
    }
    if mode == Mode::Arithmetic && c == b'\'' as u16 {
        return scan_bash_span(input, index + 1, depth, ctx, commands, &BASH_SPANS_ARITHMETIC_QUOTE, allow_bracket, text)
            .map(Some);
    }
    if mode == Mode::Word && opener == Some("$\"") {
        return scan_bash_span(input, index + 2, depth, ctx, commands, &BASH_SPANS_DOUBLE, allow_bracket, text).map(Some);
    }
    if mode != Mode::Quoted && c == b'"' as u16 {
        let span = if mode == Mode::Arithmetic { &BASH_SPANS_ARITHMETIC_DOUBLE } else { &BASH_SPANS_DOUBLE };
        return scan_bash_span(input, index + 1, depth, ctx, commands, span, allow_bracket, text).map(Some);
    }
    let end = if opener == Some("<(") || opener == Some(">(") {
        Some(scan_bash_nested(input, index + 2, depth, ctx, commands, Close::Paren, None)?)
    } else if (opener.is_some() && opener != Some("$'") && opener != Some("$\"")) || c == b'`' as u16 {
        Some(scan_bash_dollar_or_backtick(
            input,
            index,
            depth,
            ctx,
            commands,
            mode != Mode::Word,
            allow_bracket,
            text.as_deref_mut(),
        )?)
    } else {
        None
    };
    if end.is_none() && c == b'$' as u16 && is_parameter_char(next) {
        if let Some(text) = text.as_deref_mut() {
            text.word.push(b'$' as u16);
        }
        return Ok(Some(index));
    }
    if let Some(text) = text
        && let Some(end) = end
    {
        text.word.extend_from_slice(slice(input, index, end + 1));
    }
    Ok(end)
}

// Zsh flag groups such as ${(e)name} evaluate a value, and a modifier run with ~, such as $^~name or
// ${(f)~name}, globs it, which can run glob qualifier code.
fn zsh_evaluates(input: &[u16], index: isize) -> bool {
    let mut position = index + 1;
    if starts_with_ascii(input, "{\\\n", position) {
        return true;
    }
    if js::is(at(input, position), b'{') && js::is(at(input, position + 1), b'(') {
        match zsh_safe_flags(input, index) {
            None => return true,
            Some(end) => position = end,
        }
    }
    if js::is(at(input, position), b'{') {
        position += 1;
    }
    zsh_glob_modifier(input, position)
}

fn bash_append(text: &mut Text, value: &[u16]) {
    text.word.extend_from_slice(value);
    text.literal.extend_from_slice(value);
}

// Arithmetic evaluation expands `name[subscript]` again, so a literal expansion in text the shell evaluates as
// arithmetic can run: (( )), $(( )), $[ ], subscripts, ${name:offset:length}, let operands, and the arithmetic
// operands of [[ ]]. Builtins that name or bind a variable from an operand evaluate its subscript, so a literal
// expansion there runs when the operand also has a bracket; a command substitution's words stand in for the
// output it supplies. Values bound for later, such as assignment values, array elements, for lists, and
// here-strings, reach arithmetic or subscript evaluation when a variable sink appears in the command.
// A parameter's words count as its value. Text that reaches arithmetic only at runtime, through the
// environment, a function argument, or the output a command substitution supplies to a later binding, remains
// out of reach.
fn bash_evaluates_expansion(text: &Text, evaluation: Eval, ctx: &mut Ctx) -> bool {
    let combined = js::concat(&[&text.literal, &[b' ' as u16], &text.output]);
    let has_expansion = has_bash_expansion(&combined);
    // Escapes reach the value through $'...' or a substitution's printf, echo -e, or tr.
    let has_escape = has_bash_escape(&combined);
    let bracket = b'[' as u16;
    if has_expansion || has_escape || text.literal.iter().any(|&c| set_has("$`", c)) {
        ctx.bound_dollar = true;
    }
    if has_expansion || has_escape || text.literal.iter().any(|&c| set_has("({`", c)) {
        ctx.bound_open = true;
    }
    // A substitution's output can hold `[` that the script never spells (printf '\x5b', tr, base64 -d).
    if has_expansion || has_escape || !js::trim(&text.output).is_empty() || combined.contains(&bracket) {
        ctx.bound_bracket = true;
    }
    if evaluation == Eval::Deferred {
        return false;
    }
    if text.word.contains(&bracket) || combined.contains(&bracket) {
        ctx.sink_bracket = true;
    }
    let sink = if evaluation == Eval::Arithmetic {
        arithmetic_names_variable(&text.word)
    } else {
        text.word.contains(&(b'$' as u16)) || combined.contains(&bracket)
    };
    if sink {
        ctx.variable_sink = true;
    }
    if evaluation == Eval::Arithmetic {
        return has_expansion;
    }
    combined.contains(&bracket) && has_expansion
}

// Scans a subscript after its `[`; indexed arrays evaluate it as arithmetic.
fn scan_bash_subscript(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    span: &Span,
    text: &mut Text,
) -> Res<isize> {
    let end = scan_bash_span(input, start, depth, ctx, commands, span, false, Some(&mut *text))?;
    ctx.sink_bracket = true;
    if bash_evaluates_expansion(text, Eval::Arithmetic, ctx) {
        return Err(Reason::DynamicExecution);
    }
    Ok(end)
}

// Scans to the span's unnested close character and returns its index; a span without one runs to the end.
#[allow(clippy::too_many_arguments)]
fn scan_bash_span(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    span: &Span,
    allow_bracket: bool,
    mut text: Option<&mut Text>,
) -> Res<isize> {
    let mut nesting: isize = 0;
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        if Some(c) == span.close && nesting == 0 {
            return Ok(index);
        }
        // A `#` after a digit is a base prefix, and after `$` the parameter $#.
        if set_has(span.reject, c)
            && !(c == b'#' as u16 && at(input, index - 1).is_some_and(|p| js::is_digit(p) || p == b'$' as u16))
        {
            return Err(Reason::InvalidStructure);
        }
        if Some(c) == span.open {
            nesting += 1;
            if nesting + depth > MAX_SUBSTITUTION_DEPTH {
                return Err(Reason::InvalidStructure);
            }
        }
        if Some(c) == span.close {
            nesting -= 1;
        }
        let unit = if Some(c) == span.open || Some(c) == span.close {
            None
        } else {
            scan_bash_unit(input, index, depth, ctx, commands, span.mode, allow_bracket, text.as_deref_mut())?
        };
        if let Some(unit) = unit {
            index = unit;
        } else if c == b'\\' as u16 && (at(input, index + 1) == span.open || at(input, index + 1) == span.close) {
            if let Some(text) = text.as_deref_mut() {
                bash_append(text, slice(input, index, index + 2));
            }
            index += 1;
        } else if let Some(text) = text.as_deref_mut() {
            bash_append(text, &[c]);
        }
    }
    if span.close.is_some() { Err(Reason::UnterminatedQuote) } else { Ok(n) }
}

// Scans a nested list such as a command or process substitution and returns its closing index.
fn scan_bash_nested(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    close: Close,
    text: Option<&mut Text>,
) -> Res<isize> {
    let (nested, end) = scan_bash(input, start, depth + 1, ctx, Some(close), None, text)?;
    commands.extend(nested);
    Ok(end)
}

// Returns the first listed operator at index. Shells disagree about operators split by line continuations.
fn bash_operator(input: &[u16], index: isize, operators: &[&'static str]) -> Res<Option<&'static str>> {
    let mut text: Vec<u16> = Vec::with_capacity(3);
    let mut cursor = index;
    let n = len(input);
    while text.len() < 3 && cursor < n {
        if starts_with_ascii(input, "\\\n", cursor) {
            cursor += 1;
        } else {
            text.push(input[cursor as usize]);
        }
        cursor += 1;
    }
    let operator = operators.iter().copied().find(|candidate| starts_with_ascii(&text, candidate, 0));
    if let Some(operator) = operator
        && !starts_with_ascii(input, operator, index)
    {
        return Err(Reason::InvalidStructure);
    }
    Ok(operator)
}

#[allow(clippy::too_many_arguments)]
fn scan_bash_dollar_or_backtick(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    quoted: bool,
    allow_bracket: bool,
    text: Option<&mut Text>,
) -> Res<isize> {
    if depth >= MAX_SUBSTITUTION_DEPTH {
        return Err(Reason::CommandSubstitution);
    }
    if js::is(at(input, start), b'`') {
        return scan_bash_backtick(input, start, depth + 1, ctx, commands, quoted, text);
    }
    if starts_with_ascii(input, "$((", start)
        && let Some(end) = scan_bash_arithmetic(input, start + 3, depth + 1, ctx, commands, &BASH_SPANS_ARITHMETIC)?
    {
        return Ok(end);
    }
    if starts_with_ascii(input, "$(", start) {
        return scan_bash_nested(input, start + 2, depth, ctx, commands, Close::Paren, text);
    }
    if let Some(length) = nofork_open(input, start) {
        return scan_bash_nested(input, start + length, depth, ctx, commands, Close::Nofork, text);
    }
    if starts_with_ascii(input, "${", start) {
        // Literal text in parameter words reaches the enclosing word's value.
        let mut parameter = Text::default();
        let end = scan_bash_parameter(input, start + 2, depth + 1, ctx, commands, quoted, &mut parameter);
        // ${!name} and Zsh ${(P)name} evaluate the subscript in the referenced name.
        let indirect = js::is(at(input, skip_continuations(input, start + 2)), b'!')
            || (ctx.dialect != Dialect::Bash
                && js::is(at(input, start + 2), b'(')
                && zsh_safe_flags(input, start).is_some_and(|flags_end| {
                    js::includes(&strip_zsh_quoted_flag_args(slice(input, start + 3, flags_end - 1)), b'P' as u16)
                }));
        if indirect {
            ctx.variable_sink = true;
            if bash_evaluates_expansion(&parameter, Eval::Binding, ctx) {
                return Err(Reason::DynamicExecution);
            }
        }
        if let Some(text) = text {
            text.literal.extend_from_slice(&parameter.literal);
            text.output.extend_from_slice(&parameter.output);
        }
        return end;
    }
    // Dash leaves `$[` literal, which splits words differently; Bash and Zsh always read arithmetic.
    if !allow_bracket && ctx.dialect == Dialect::Posix {
        return Err(Reason::CommandSubstitution);
    }
    let mut expression = Text::default();
    let end = scan_bash_span(
        input,
        start + 2,
        depth + 1,
        ctx,
        commands,
        &BASH_SPANS_BRACKET_ARITHMETIC,
        true,
        Some(&mut expression),
    )?;
    if bash_evaluates_expansion(&expression, Eval::Arithmetic, ctx) {
        return Err(Reason::DynamicExecution);
    }
    Ok(end)
}

// Scans the text of ((...)) after its opening parentheses. Returns None when the text closes with a single
// parenthesis, which makes it a nested subshell instead.
fn scan_bash_arithmetic(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    span: &Span,
) -> Res<Option<isize>> {
    let mut arithmetic = Vec::new();
    let mut text = Text::default();
    let Ok(end) = scan_bash_span(input, start, depth, ctx, &mut arithmetic, span, true, Some(&mut text)) else {
        return Err(Reason::InvalidStructure);
    };
    if bash_operator(input, end, &["))"])?.is_none() {
        return Ok(None);
    }
    if bash_evaluates_expansion(&text, Eval::Arithmetic, ctx) {
        return Err(Reason::DynamicExecution);
    }
    commands.extend(arithmetic);
    Ok(Some(end + 1))
}

fn scan_bash_backtick(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    quoted: bool,
    text: Option<&mut Text>,
) -> Res<isize> {
    let mut source = JsString::new();
    let n = len(input);
    let mut index = start;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        if c == b'`' as u16 {
            let Ok((inner, _)) = scan_bash(&source, 0, depth, ctx, None, None, text) else {
                return Err(Reason::CommandSubstitution);
            };
            commands.extend(inner);
            return Ok(index);
        }
        let next = at(input, index + 1).unwrap_or(0);
        if c == b'\\' as u16 && (set_has("$`\\\n", next) || (quoted && next == b'"' as u16)) {
            index += 1;
            if next != b'\n' as u16 {
                source.push(next);
            }
            continue;
        }
        source.push(c);
    }
    Err(Reason::CommandSubstitution)
}

fn scan_bash_parameter(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    quoted: bool,
    text: &mut Text,
) -> Res<isize> {
    let name_start = parameter_prefix(input, start).unwrap_or(start);
    // The operands of ${name:offset:length} are arithmetic, and ${name:=word} or ${name=word} binds a variable.
    let mut colon = parameter_name(input, name_start).unwrap_or(-1);
    let name = if colon > name_start {
        js::replace_seq(slice(input, name_start, colon), &units("\\\n"), &[])
    } else {
        JsString::new()
    };
    if ctx.dialect != Dialect::Bash
        && (js::eq_ascii(&name, "NULLCMD")
            || js::eq_ascii(&name, "READNULLCMD")
            || (zsh_dynamic_parameter(&name) && set_has_or_empty("[=:", at(input, colon))))
    {
        return Err(Reason::DynamicExecution);
    }
    if js::eq_ascii(&name, "PS4") {
        ctx.eval_sink = true;
    }
    let mut evaluation: Option<Eval> = None;
    let mut word_offset = 0;
    let mut literal_offset = 0;
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        if c == b'}' as u16 {
            if let Some(evaluation) = evaluation {
                let operand = Text {
                    word: text.word[word_offset..].to_vec(),
                    literal: text.literal[literal_offset..].to_vec(),
                    output: text.output.clone(),
                };
                if bash_evaluates_expansion(&operand, evaluation, ctx) {
                    return Err(Reason::DynamicExecution);
                }
            }
            return Ok(index);
        }
        if index == colon && evaluation.is_none() {
            if c == b':' as u16 && !set_has_or_empty("-=?+", at(input, index + 1)) {
                evaluation = Some(Eval::Arithmetic);
            }
            if c == b'=' as u16 || (c == b':' as u16 && js::is(at(input, index + 1), b'=')) {
                evaluation = Some(Eval::Deferred);
            }
            if evaluation.is_some() {
                word_offset = text.word.len();
                literal_offset = text.literal.len();
            }
        }
        // Bash ${name@P} evaluates the value as prompt text.
        if c == b'@' as u16 && parameter_prompt(input, index) {
            return Err(Reason::DynamicExecution);
        }
        // Zsh globs an unquoted parameter's words, where a parenthesized group can be a glob qualifier.
        if !quoted && ctx.dialect != Dialect::Bash && c == b'(' as u16 && index > start {
            return Err(Reason::DynamicExecution);
        }
        if quoted && c == b'\'' as u16 && js::is(at(input, index - 1), b'$') {
            let Some(ansi) = bash_ansi_quote(input, index, true) else {
                return Err(Reason::UnterminatedQuote);
            };
            text.literal.extend_from_slice(&ansi.value);
        }
        let subscript = index == colon && c == b'[' as u16;
        let unit = if subscript {
            Some(scan_bash_subscript(input, index + 1, depth, ctx, commands, &BASH_SPANS_SUBSCRIPT, &mut Text::default())?)
        } else if quoted && c == b'\'' as u16 {
            Some(scan_bash_span(input, index + 1, depth, ctx, commands, &BASH_SPANS_PARAMETER_QUOTE, false, Some(text))?)
        } else if quoted && c == b'"' as u16 {
            Some(scan_bash_span(input, index + 1, depth, ctx, commands, &BASH_SPANS_DOUBLE, false, Some(text))?)
        } else {
            let mode = if quoted { Mode::Quoted } else { Mode::Word };
            scan_bash_unit(input, index, depth, ctx, commands, mode, false, Some(text))?
        };
        // Bash 3.2 ends a double-quoted parameter at an unquoted `}` inside `$(...)`.
        if quoted
            && let Some(unit) = unit
            && starts_with_ascii(input, "$(", index)
            && js::includes(slice(input, index, unit + 1), b'}' as u16)
        {
            return Err(Reason::CommandSubstitution);
        }
        if let Some(unit) = unit
            && (subscript || (index == name_start && starts_with_ascii(input, "${", index)))
        {
            colon = line_continuations(input, unit + 1).unwrap_or(unit + 1);
        }
        match unit {
            Some(unit) => index = unit,
            None => bash_append(text, &[c]),
        }
    }
    Err(Reason::CommandSubstitution)
}

fn scan_bash_conditional(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
) -> Res<isize> {
    let mut words: Vec<Text> = Vec::new();
    let mut text = Text::default();
    let mut word_started = false;
    let mut has_word = false;
    let mut paren_depth: isize = 0;
    let mut pattern_paren_depth: isize = 0;
    let flush_word = |words: &mut Vec<Text>, text: &mut Text, has_word: &mut bool| {
        if !*has_word {
            return;
        }
        words.push(std::mem::take(text));
        *has_word = false;
    };
    let single = |c: u16| Text { word: vec![c], literal: JsString::new(), output: JsString::new() };
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        if c == b'\\' as u16 && js::is(at(input, index + 1), b'\n') {
            index += 1;
            continue;
        }
        if !word_started {
            flush_word(&mut words, &mut text, &mut has_word);
        }
        let close = if !word_started && c == b']' as u16 { bash_operator(input, index, &["]]"])? } else { None };
        if close.is_some() && brace_close_ahead(input, index + 2) {
            if paren_depth != 0 {
                return Err(Reason::InvalidStructure);
            }
            // -v names a variable, -t in Zsh evaluates the descriptor, and arithmetic comparisons evaluate both operands.
            let word_is = |at: isize, value: &str| word_at_text(&words, at).is_some_and(|w| js::eq_ascii(&w.word, value));
            let comparison = |at: isize| {
                word_at_text(&words, at).is_some_and(|w| has(BASH_ARITHMETIC_COMPARISONS, &w.word))
            };
            let mut evaluated = false;
            for (position, word) in words.iter().enumerate() {
                let position = position as isize;
                if (word_is(position - 1, "-v") && bash_evaluates_expansion(word, Eval::Binding, ctx))
                    || (((ctx.dialect != Dialect::Bash && word_is(position - 1, "-t"))
                        || comparison(position - 1)
                        || comparison(position + 1))
                        && bash_evaluates_expansion(word, Eval::Arithmetic, ctx))
                {
                    evaluated = true;
                    break;
                }
            }
            if evaluated {
                return Err(Reason::DynamicExecution);
            }
            return Ok(index + 1);
        }
        if !word_started && c == b'#' as u16 {
            let newline = js::index_of(input, b'\n' as u16, index);
            if newline < 0 {
                return Err(Reason::InvalidStructure);
            }
            index = newline;
            continue;
        }
        if c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 {
            if pattern_paren_depth == 0 {
                word_started = false;
            }
            continue;
        }
        if c == b';' as u16 {
            return Err(Reason::InvalidStructure);
        }
        if c == b'(' as u16 || c == b')' as u16 {
            if c == b')' as u16 && paren_depth == 0 {
                return Err(Reason::InvalidStructure);
            }
            let in_pattern = pattern_paren_depth > 0
                || (c == b'(' as u16
                    && words.last().is_some_and(|w| ["=", "==", "!=", "=~"].iter().any(|op| js::eq_ascii(&w.word, op))));
            let step = if c == b'(' as u16 { 1 } else { -1 };
            paren_depth += step;
            if in_pattern {
                pattern_paren_depth += step;
                word_started = true;
                has_word = true;
                bash_append(&mut text, &[c]);
                continue;
            }
            flush_word(&mut words, &mut text, &mut has_word);
            words.push(single(c));
            word_started = false;
            continue;
        }
        let unit = scan_bash_unit(input, index, depth, ctx, commands, Mode::Word, true, Some(&mut text))?;
        if let Some(unit) = unit {
            index = unit;
        }
        if unit.is_none() && pattern_paren_depth == 0 && set_has("&|<>", c) {
            flush_word(&mut words, &mut text, &mut has_word);
            words.push(single(c));
            word_started = false;
            continue;
        }
        word_started = true;
        has_word = true;
        if unit.is_none() {
            bash_append(&mut text, &[c]);
        }
    }
    Err(Reason::InvalidStructure)
}

fn word_at_text(words: &[Text], index: isize) -> Option<&Text> {
    if index < 0 { None } else { words.get(index as usize) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArrayMode {
    Array,
    Pattern,
}

fn scan_bash_array_or_pattern(
    input: &[u16],
    start: isize,
    depth: isize,
    ctx: &mut Ctx,
    commands: &mut Vec<usize>,
    mode: ArrayMode,
) -> Res<isize> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        return Err(Reason::CommandSubstitution);
    }
    let array = mode == ArrayMode::Array;
    // Array elements bind variables, so their decoded text is a subscript sink.
    let mut text = if array { Some(Text::default()) } else { None };
    let mut word_started = false;
    let n = len(input);
    let mut index = start;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        ctx.remaining -= 1;
        if ctx.remaining < 0 {
            return Err(Reason::InvalidStructure);
        }
        let c = input[index as usize];
        if c == b'\\' as u16 && js::is(at(input, index + 1), b'\n') {
            index += 1;
            continue;
        }
        if let Some(text) = text.as_mut()
            && set_has(") \t\n", c)
        {
            bash_evaluates_expansion(text, Eval::Deferred, ctx);
            text.literal.clear();
            text.output.clear();
        }
        if c == b')' as u16 {
            return Ok(index);
        }
        if array && !word_started && c == b'#' as u16 {
            let newline = js::index_of(input, b'\n' as u16, index);
            if newline < 0 {
                return Err(Reason::CommandSubstitution);
            }
            index = newline;
            continue;
        }
        if c == b' ' as u16 || c == b'\t' as u16 || (array && c == b'\n' as u16) {
            word_started = false;
            continue;
        }
        if array
            && word_started
            && c == b'(' as u16
            && ctx.dialect == Dialect::Zsh
            && let Some(qualifier_end) = zsh_glob_qualifier(input, index)
        {
            let end = qualifier_end - 1;
            if let Some(text) = text.as_mut() {
                text.literal.extend_from_slice(slice(input, index, end + 1));
            }
            index = end;
            continue;
        }
        // Array elements are globbed, where a parenthesized group can be a Zsh glob qualifier.
        if c == b'\n' as u16 || c == b';' as u16 || c == b'&' as u16 || (array && set_has("|(", c)) {
            return Err(Reason::CommandSubstitution);
        }
        word_started = true;
        let unit = if array && c == b'=' as u16 && js::is(at(input, index + 1), b'(') {
            Some(scan_bash_nested(input, index + 2, depth, ctx, commands, Close::Paren, None)?)
        } else if c == b'(' as u16 {
            Some(scan_bash_array_or_pattern(input, index, depth + 1, ctx, commands, mode)?)
        } else if c == b'[' as u16 {
            Some(scan_bash_subscript(input, index + 1, depth, ctx, commands, &BASH_SPANS_SUBSCRIPT, &mut Text::default())?)
        } else {
            scan_bash_unit(input, index, depth, ctx, commands, Mode::Word, false, text.as_mut())?
        };
        match unit {
            Some(unit) => index = unit,
            None => {
                if let Some(text) = text.as_mut() {
                    text.literal.push(c);
                }
            }
        }
    }
    Err(Reason::CommandSubstitution)
}

struct AnsiQuote {
    value: JsString,
    end: isize,
}

fn bash_ansi_quote(input: &[u16], start: isize, allow_unicode: bool) -> Option<AnsiQuote> {
    let mut value = JsString::new();
    let n = len(input);
    let mut index = start;
    loop {
        index += 1;
        if index >= n {
            return None;
        }
        let c = input[index as usize];
        if c == b'\'' as u16 {
            return Some(AnsiQuote { value, end: index });
        }
        if c != b'\\' as u16 {
            value.push(c);
            continue;
        }
        index += 1;
        let escaped = at(input, index)?;
        if let Some(decoded) = ansi_escape(escaped) {
            value.push(decoded);
            continue;
        }
        let octal = (b'0' as u16..=b'7' as u16).contains(&escaped);
        let digits = if escaped == b'x' as u16 {
            leading(slice(input, index + 1, index + 3), js::is_hex)
        } else if allow_unicode && escaped == b'u' as u16 {
            leading(slice(input, index + 1, index + 5), js::is_hex)
        } else if allow_unicode && escaped == b'U' as u16 {
            leading(slice(input, index + 1, index + 9), js::is_hex)
        } else if octal {
            leading(slice(input, index, index + 3), |c| (b'0' as u16..=b'7' as u16).contains(&c))
        } else {
            &[]
        };
        if digits.is_empty() {
            return None;
        }
        let radix = if octal { 8 } else { 16 };
        let point = digits.iter().fold(0u64, |acc, &d| acc * radix + (char::from(d as u8).to_digit(radix as u32).unwrap() as u64));
        if point == 0 || point > 0x10ffff || (0xd800..=0xdfff).contains(&point) {
            return None;
        }
        js::push_code_point(&mut value, point as u32);
        index += digits.len() as isize - if octal { 1 } else { 0 };
    }
}

fn leading(text: &[u16], class: impl Fn(u16) -> bool) -> &[u16] {
    let end = text.iter().position(|&c| !class(c)).unwrap_or(text.len());
    &text[..end]
}

fn ansi_escape(c: u16) -> Option<u16> {
    let decoded = match u8::try_from(c).ok()? {
        b'a' => 0x07,
        b'b' => 0x08,
        b'e' | b'E' => 0x1b,
        b'f' => 0x0c,
        b'n' => 0x0a,
        b'r' => 0x0d,
        b't' => 0x09,
        b'v' => 0x0b,
        b'\\' => b'\\',
        b'\'' => b'\'',
        b'"' => b'"',
        b'?' => b'?',
        _ => return None,
    };
    Some(decoded as u16)
}

struct HeredocDelimiter {
    delimiter: JsString,
    dash_delimiter: Option<JsString>,
    quoted: bool,
    tabs: bool,
    end: isize,
}

fn bash_heredoc_delimiter(input: &[u16], start: isize, tabs: bool) -> Option<HeredocDelimiter> {
    let mut delimiter = JsString::new();
    let mut dash_delimiter: Option<JsString> = None;
    let mut quoted = false;
    let mut quote: Option<u16> = None;
    let mut end = start;
    let mut started = false;
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        let c = input[index as usize];
        if !started && (c == b' ' as u16 || c == b'\t' as u16) {
            continue;
        }
        if !started && c == b'#' as u16 {
            return None;
        }
        if quote.is_none() && set_has(" \t\n;&|()<>", c) {
            return started.then_some(HeredocDelimiter { delimiter, dash_delimiter, quoted, tabs, end });
        }
        if quote.is_none() && c == b'$' as u16 && starts_with_ascii(input, "\\\n", index + 1) {
            return None;
        }
        // Dash retains $ in $'...' heredoc delimiters while Bash and Zsh strip $.
        if quote.is_none() && starts_with_ascii(input, "$'", index) {
            let literal = bash_ansi_quote(input, index + 1, false)?;
            if literal.end != js::index_of(input, b'\'' as u16, index + 2) {
                return None;
            }
            let mut dash = dash_delimiter.take().unwrap_or_else(|| delimiter.clone());
            dash.push(b'$' as u16);
            dash.extend_from_slice(slice(input, index + 2, literal.end));
            dash_delimiter = Some(dash);
            delimiter.extend_from_slice(&literal.value);
            quoted = true;
            started = true;
            index = literal.end;
            end = index;
            continue;
        }
        // Bash strips $ from $"..." heredoc delimiters while Zsh and Dash retain $.
        if quote.is_none() && starts_with_ascii(input, "$\"", index) {
            return None;
        }
        if Some(c) == quote {
            quote = None;
            end = index;
            continue;
        }
        if c == b'\\' as u16 && quote != Some(b'\'' as u16) {
            let next = at(input, index + 1)?;
            if next == b'\n' as u16 {
                index += 1;
                continue;
            }
            // Double quotes only remove escapes for shell-special characters.
            if quote.is_none() || set_has("$`\"\\", next) {
                quoted = true;
                started = true;
                index += 1;
                let escaped = input[index as usize];
                if let Some(dash) = dash_delimiter.as_mut() {
                    dash.push(escaped);
                }
                delimiter.push(escaped);
                end = index;
                continue;
            }
        }
        if quote.is_none() && (c == b'\'' as u16 || c == b'"' as u16) {
            quote = Some(c);
            quoted = true;
            started = true;
            end = index;
            continue;
        }
        if let Some(dash) = dash_delimiter.as_mut() {
            dash.push(c);
        }
        delimiter.push(c);
        started = true;
        end = index;
    }
    (started && quote.is_none()).then_some(HeredocDelimiter { delimiter, dash_delimiter, quoted, tabs, end })
}

struct HeredocBody {
    source: JsString,
    end: isize,
}

fn bash_heredoc(input: &[u16], start: isize, delimiter: &Heredoc) -> Option<HeredocBody> {
    let body_start = start;
    let mut start = start;
    let mut line_start = start;
    let mut line = JsString::new();
    // Backslashes ending the joined line; an odd count continues it.
    let mut backslashes: isize = 0;
    let n = len(input);
    let max_length = delimiter.delimiter.len().max(delimiter.dash_delimiter.as_ref().map_or(0, |d| d.len()));
    let mut index = start - 1;
    loop {
        index += 1;
        if index > n {
            break;
        }
        if index < n && input[index as usize] != b'\n' as u16 {
            continue;
        }
        let raw = slice(input, start, index);
        let text = if delimiter.tabs && start == line_start {
            &raw[raw.iter().take_while(|&&c| c == b'\t' as u16).count()..]
        } else {
            raw
        };
        let run = text.iter().rev().take_while(|&&c| c == b'\\' as u16).count() as isize;
        backslashes = if run == len(text) { backslashes + run } else { run };
        let continued = !delimiter.quoted && backslashes % 2 == 1 && index < n;
        // Joined lines only grow, so text past the delimiter's length cannot change the comparison.
        if line.len() <= max_length {
            line.extend_from_slice(if continued { slice(text, 0, -1) } else { text });
        }
        if continued {
            backslashes -= 1;
            start = index + 1;
            continue;
        }
        if delimiter.dash_delimiter.as_ref().is_some_and(|dash| *dash == line) {
            return None;
        }
        if line == delimiter.delimiter {
            // Dash does not join backslash-continued delimiter lines; fail closed if a physical delimiter follows.
            if start > line_start && js::includes_seq(js::slice_from(input, index + 1), &delimiter.delimiter) {
                return None;
            }
            return Some(HeredocBody { source: slice(input, body_start, line_start).to_vec(), end: index });
        }
        line.clear();
        backslashes = 0;
        start = index + 1;
        line_start = start;
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// The regular expressions of scan.ts. A sticky regex tested at a position past the end fails (`lastIndex` beyond
// the string), so every matcher rejects `i > len`.

/// `(?:\\\n)*` from `i`.
fn skip_continuations(s: &[u16], mut i: isize) -> isize {
    while starts_with_ascii(s, "\\\n", i) {
        i += 2;
    }
    i
}

/// `(?:[ \t]|\\\n)*` from `i`.
fn skip_blanks(s: &[u16], mut i: isize) -> isize {
    loop {
        if js::set_has_opt(" \t", at(s, i)) {
            i += 1;
        } else if starts_with_ascii(s, "\\\n", i) {
            i += 2;
        } else {
            return i;
        }
    }
}

/// `LINE_CONTINUATION_RE = /(?:\\\n)*/y`: the `lastIndex` after it.
fn line_continuations(s: &[u16], i: isize) -> Option<isize> {
    if i > len(s) { None } else { Some(skip_continuations(s, i)) }
}

/// `TOKEN_RE = /[A-Za-z_][A-Za-z0-9_]*(?=(?:\\\n)*(?:[ \t\n;|&()<>]|$))/y`
fn token_at(s: &[u16], i: isize) -> Option<&[u16]> {
    if !at(s, i).is_some_and(js::is_ident_start) {
        return None;
    }
    let mut j = i + 1;
    while at(s, j).is_some_and(js::is_word) {
        j += 1;
    }
    let k = skip_continuations(s, j);
    if k == len(s) || set_has(" \t\n;|&()<>", s[k as usize]) { Some(slice(s, i, j)) } else { None }
}

/// `BRACE_CLOSE_AHEAD_RE = /(?:\\\n)*(?:[ \t\n;&|()<>]|$)/y`
fn brace_close_ahead(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    let k = skip_continuations(s, i);
    k == len(s) || set_has(" \t\n;&|()<>", s[k as usize])
}

/// `SPACE_CONTINUATION_AHEAD_RE = /(?:\\\n)*[ \t\n]/y`
fn space_continuation_ahead(s: &[u16], i: isize) -> bool {
    i <= len(s) && js::set_has_opt(" \t\n", at(s, skip_continuations(s, i)))
}

/// `NEGATION_AHEAD_RE = /(?:\\\n)*[ \t\n(]/y`
fn negation_ahead(s: &[u16], i: isize) -> bool {
    i <= len(s) && js::set_has_opt(" \t\n(", at(s, skip_continuations(s, i)))
}

/// `COMPOUND_AHEAD`: `(?=(?:!(?:[ \t]|\\\n)*)*(?:[{(]|\[\[(?:\\\n)*[ \t\n]|(?:if|while|until|for|select|case|repeat)\b))`
fn compound_ahead(s: &[u16], mut i: isize) -> bool {
    while js::is(at(s, i), b'!') {
        i = skip_blanks(s, i + 1);
    }
    match at(s, i) {
        Some(c) if c == b'{' as u16 || c == b'(' as u16 => true,
        Some(c) if c == b'[' as u16 && js::is(at(s, i + 1), b'[') => {
            js::set_has_opt(" \t\n", at(s, skip_continuations(s, i + 2)))
        }
        _ => COMPOUND_KEYWORDS.iter().any(|keyword| {
            starts_with_ascii(s, keyword, i) && !at(s, i + keyword.len() as isize).is_some_and(js::is_word)
        }),
    }
}

/// `COPROC_AHEAD_RE`: `coproc(?:[ \t]|\\\n)+(?:--(?:[ \t]|\\\n)+)?(?:[A-Za-z_][A-Za-z0-9_]*(?:[ \t]|\\\n)+)?` then
/// `COMPOUND_AHEAD`; the match length.
fn coproc_ahead(s: &[u16], i: isize) -> Option<isize> {
    if !starts_with_ascii(s, "coproc", i) {
        return None;
    }
    let mut j = skip_blanks(s, i + 6);
    if j == i + 6 {
        return None;
    }
    if starts_with_ascii(s, "--", j) {
        let k = skip_blanks(s, j + 2);
        if k > j + 2 {
            j = k;
        }
    }
    if at(s, j).is_some_and(js::is_ident_start) {
        let mut name_end = j + 1;
        while at(s, name_end).is_some_and(js::is_word) {
            name_end += 1;
        }
        let k = skip_blanks(s, name_end);
        if k > name_end && compound_ahead(s, k) {
            return Some(k - i);
        }
    }
    compound_ahead(s, j).then_some(j - i)
}

/// `TIME_AHEAD_RE`: `time(?:[ \t]|\\\n)+(?:-p(?:[ \t]|\\\n)+)?(?:--(?:[ \t]|\\\n)+)?` then `COMPOUND_AHEAD`.
fn time_ahead(s: &[u16], i: isize) -> Option<isize> {
    if !starts_with_ascii(s, "time", i) {
        return None;
    }
    let mut j = skip_blanks(s, i + 4);
    if j == i + 4 {
        return None;
    }
    for option in ["-p", "--"] {
        if starts_with_ascii(s, option, j) {
            let k = skip_blanks(s, j + 2);
            if k > j + 2 {
                j = k;
            }
        }
    }
    compound_ahead(s, j).then_some(j - i)
}

struct FunctionHead {
    length: isize,
    function: bool,
    name: Option<(isize, isize)>,
    parens: bool,
    trailing: isize,
}

/// `FUNCTION_HEAD_RE = /(function[ \t](?:[ \t]|\\\n)*)?([A-Za-z_][\w.:+@%-]*)?(?:[ \t]|\\\n)*(\([ \t]*\))?((?:[ \t\n]|\\\n|#[^\n]*\n)*)/y`
fn function_head(s: &[u16], start: isize) -> Option<FunctionHead> {
    if start > len(s) {
        return None;
    }
    let mut j = start;
    let mut function = false;
    if starts_with_ascii(s, "function", j) && js::set_has_opt(" \t", at(s, j + 8)) {
        j = skip_blanks(s, j + 9);
        function = true;
    }
    let mut name = None;
    if at(s, j).is_some_and(js::is_ident_start) {
        let mut k = j + 1;
        while at(s, k).is_some_and(|c| js::is_word(c) || set_has(".:+@%-", c)) {
            k += 1;
        }
        name = Some((j, k));
        j = k;
    }
    j = skip_blanks(s, j);
    let mut parens = false;
    if js::is(at(s, j), b'(') {
        let mut k = j + 1;
        while js::set_has_opt(" \t", at(s, k)) {
            k += 1;
        }
        if js::is(at(s, k), b')') {
            parens = true;
            j = k + 1;
        }
    }
    let trailing_start = j;
    loop {
        if js::set_has_opt(" \t\n", at(s, j)) {
            j += 1;
        } else if starts_with_ascii(s, "\\\n", j) {
            j += 2;
        } else if js::is(at(s, j), b'#') {
            let newline = js::index_of(s, b'\n' as u16, j);
            if newline < 0 {
                break;
            }
            j = newline + 1;
        } else {
            break;
        }
    }
    Some(FunctionHead { length: j - start, function, name, parens, trailing: j - trailing_start })
}

/// `FUNCTION_BODY_RE = /[{(]|\[\[(?=(?:\\\n)*[ \t\n])|(?:if|while|until|for|select|case|repeat)(?=(?:\\\n)*(?:[ \t\n(]|$))/y`
fn function_body(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    match at(s, i) {
        Some(c) if c == b'{' as u16 || c == b'(' as u16 => true,
        Some(c) if c == b'[' as u16 && js::is(at(s, i + 1), b'[') && space_continuation_ahead(s, i + 2) => true,
        _ => COMPOUND_KEYWORDS.iter().any(|keyword| {
            starts_with_ascii(s, keyword, i) && {
                let k = skip_continuations(s, i + keyword.len() as isize);
                k == len(s) || set_has(" \t\n(", s[k as usize])
            }
        }),
    }
}

/// `DO_AHEAD_RE = /(?:[ \t\n;]|\\\n|#[^\n]*(?:\n|$))*(?:do(?=(?:\\\n)*(?:[ \t\n;{(]|$))|\{(?=(?:\\\n)*[ \t\n]))/y`
fn do_ahead(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    let mut j = i;
    loop {
        if js::set_has_opt(" \t\n;", at(s, j)) {
            j += 1;
        } else if starts_with_ascii(s, "\\\n", j) {
            j += 2;
        } else if js::is(at(s, j), b'#') {
            let newline = js::index_of(s, b'\n' as u16, j);
            j = if newline < 0 { len(s) } else { newline + 1 };
        } else {
            break;
        }
    }
    if starts_with_ascii(s, "do", j) {
        let k = skip_continuations(s, j + 2);
        if k == len(s) || set_has(" \t\n;{(", s[k as usize]) {
            return true;
        }
    }
    js::is(at(s, j), b'{') && space_continuation_ahead(s, j + 1)
}

/// `NOFORK_OPEN_RE = /\$\{(?:\\\n)*(?:[ \t\n]|\|)/y`: the match length.
fn nofork_open(s: &[u16], i: isize) -> Option<isize> {
    if !starts_with_ascii(s, "${", i) {
        return None;
    }
    let k = skip_continuations(s, i + 2);
    js::set_has_opt(" \t\n|", at(s, k)).then_some(k + 1 - i)
}

/// `ZSH_SAFE_FLAGS_RE = /\$\{\((?:[@AaBbCcDEfFikLMmNnOopPqStUuVvWwXZz0-]|q[+]|[js]([^({[<\\])(?:(?!\1)[^)])*\1)*\)/y`:
/// the `lastIndex` after it. Backtracking cannot change the outcome: `q` before `+` can only be `q[+]`, since `+`
/// starts no flag, and a delimited argument ends at the first delimiter.
fn zsh_safe_flags(s: &[u16], i: isize) -> Option<isize> {
    if !starts_with_ascii(s, "${(", i) {
        return None;
    }
    let mut j = i + 3;
    loop {
        let c = at(s, j)?;
        if c == b')' as u16 {
            return Some(j + 1);
        }
        if c == b'q' as u16 && js::is(at(s, j + 1), b'+') {
            j += 2;
            continue;
        }
        if set_has("@AaBbCcDEfFikLMmNnOopPqStUuVvWwXZz0-", c) {
            j += 1;
            continue;
        }
        if c == b'j' as u16 || c == b's' as u16 {
            let delimiter = at(s, j + 1)?;
            if set_has("({[<\\", delimiter) {
                return None;
            }
            let mut k = j + 2;
            while at(s, k).is_some_and(|x| x != delimiter && x != b')' as u16) {
                k += 1;
            }
            if at(s, k) == Some(delimiter) {
                j = k + 1;
                continue;
            }
        }
        return None;
    }
}

/// `text.replace(ZSH_QUOTED_FLAG_ARG_RE, "")` with `ZSH_QUOTED_FLAG_ARG_RE = /[js]([^({[<\\])(?:(?!\1)[^)])*\1/g`.
fn strip_zsh_quoted_flag_args(s: &[u16]) -> JsString {
    let mut out = JsString::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if (c == b'j' as u16 || c == b's' as u16)
            && let Some(&delimiter) = s.get(i + 1)
            && !set_has("({[<\\", delimiter)
        {
            let mut k = i + 2;
            while k < s.len() && s[k] != delimiter && s[k] != b')' as u16 {
                k += 1;
            }
            if k < s.len() && s[k] == delimiter {
                i = k + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `ZSH_GLOB_MODIFIER_RE = /(?:[\^=~#+]|\\\n)*~/y`: a `~` within the run of modifiers.
fn zsh_glob_modifier(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    let mut j = i;
    loop {
        if js::is(at(s, j), b'~') {
            return true;
        }
        if js::set_has_opt("^=#+", at(s, j)) {
            j += 1;
        } else if starts_with_ascii(s, "\\\n", j) {
            j += 2;
        } else {
            return false;
        }
    }
}

/// `ZSH_GLOB_QUALIFIER_RE`: `\((?:[/F.@=*rwxAIERWsStUG^MTNDn-]|[oO][nLlamcdN]|[amc][Mwhmsd]?[-+]?\d+|L[kKmMpPgGtT]?[-+]?\d+|[lY][-+]?\d+|\[[-+]?\d+(?:,[-+]?\d+)?\]|,)+\)(?=(?:\\\n)*(?:[ \t\n;&|<>)]|$))`
/// with the sticky flag: the `lastIndex` after it. The qualifiers start with distinct characters, so the greedy
/// reading is the only one.
fn zsh_glob_qualifier(s: &[u16], i: isize) -> Option<isize> {
    if !js::is(at(s, i), b'(') {
        return None;
    }
    let sign_digits = |mut k: isize| -> Option<isize> {
        if js::set_has_opt("-+", at(s, k)) {
            k += 1;
        }
        if !js::is_digit_opt(at(s, k)) {
            return None;
        }
        while js::is_digit_opt(at(s, k)) {
            k += 1;
        }
        Some(k)
    };
    let mut j = i + 1;
    let mut count = 0;
    while let Some(c) = at(s, j) {
        let next = if set_has("/F.@=*rwxAIERWsStUG^MTNDn-,", c) {
            Some(j + 1)
        } else if set_has("oO", c) {
            js::set_has_opt("nLlamcdN", at(s, j + 1)).then_some(j + 2)
        } else if set_has("amc", c) {
            let k = if js::set_has_opt("Mwhmsd", at(s, j + 1)) { j + 2 } else { j + 1 };
            sign_digits(k)
        } else if c == b'L' as u16 {
            let k = if js::set_has_opt("kKmMpPgGtT", at(s, j + 1)) { j + 2 } else { j + 1 };
            sign_digits(k)
        } else if set_has("lY", c) {
            sign_digits(j + 1)
        } else if c == b'[' as u16 {
            sign_digits(j + 1).and_then(|k| {
                let k = if js::is(at(s, k), b',') { sign_digits(k + 1).unwrap_or(k) } else { k };
                js::is(at(s, k), b']').then_some(k + 1)
            })
        } else {
            None
        };
        match next {
            Some(next) => {
                j = next;
                count += 1;
            }
            None => break,
        }
    }
    if count == 0 || !js::is(at(s, j), b')') {
        return None;
    }
    let k = skip_continuations(s, j + 1);
    (k == len(s) || set_has(" \t\n;&|<>)", s[k as usize])).then_some(j + 1)
}

/// `ZSH_SUBSCRIPT_RE = /\$(?:[\^=+~#]|\\\n)*(?:[A-Za-z_](?:[A-Za-z0-9_]|\\\n)*|[@*#?$!0-9-])(?:\\\n)*\[/y`: the
/// `lastIndex` after the `[`. `#` is both a modifier and a name, so shorter modifier runs are tried as the regex
/// backtracks.
fn zsh_subscript(s: &[u16], i: isize) -> Option<isize> {
    if !js::is(at(s, i), b'$') {
        return None;
    }
    let mut positions = vec![i + 1];
    let mut j = i + 1;
    loop {
        if js::set_has_opt("^=+~#", at(s, j)) {
            j += 1;
        } else if starts_with_ascii(s, "\\\n", j) {
            j += 2;
        } else {
            break;
        }
        positions.push(j);
    }
    let name_then_bracket = |p: isize| -> Option<isize> {
        let c = at(s, p)?;
        let mut k = p + 1;
        if js::is_ident_start(c) {
            loop {
                if at(s, k).is_some_and(js::is_word) {
                    k += 1;
                } else if starts_with_ascii(s, "\\\n", k) {
                    k += 2;
                } else {
                    break;
                }
            }
        } else if !(js::is_digit(c) || set_has("@*#?$!-", c)) {
            return None;
        }
        let k = skip_continuations(s, k);
        js::is(at(s, k), b'[').then_some(k + 1)
    };
    positions.into_iter().rev().find_map(name_then_bracket)
}

/// `PARAMETER_PREFIX_RE = /(?:\\\n)*(?:\([^)]*\))?(?:[!#^=+~]|\\\n)*/y`: the `lastIndex` after it.
fn parameter_prefix(s: &[u16], i: isize) -> Option<isize> {
    if i > len(s) {
        return None;
    }
    let mut j = skip_continuations(s, i);
    if js::is(at(s, j), b'(') {
        let close = js::index_of(s, b')' as u16, j + 1);
        if close >= 0 {
            j = close + 1;
        }
    }
    loop {
        if js::set_has_opt("!#^=+~", at(s, j)) {
            j += 1;
        } else if starts_with_ascii(s, "\\\n", j) {
            j += 2;
        } else {
            return Some(j);
        }
    }
}

/// `PARAMETER_NAME_RE = /(?:[A-Za-z_](?:[A-Za-z0-9_]|\\\n)*|\d(?:\d|\\\n)*|[@*#?$!-])(?:\\\n)*/y`: the `lastIndex`.
fn parameter_name(s: &[u16], i: isize) -> Option<isize> {
    let c = at(s, i)?;
    let mut k = i + 1;
    let run = |k: &mut isize, class: fn(u16) -> bool| loop {
        if at(s, *k).is_some_and(class) {
            *k += 1;
        } else if starts_with_ascii(s, "\\\n", *k) {
            *k += 2;
        } else {
            break;
        }
    };
    if js::is_ident_start(c) {
        run(&mut k, js::is_word);
    } else if js::is_digit(c) {
        run(&mut k, js::is_digit);
    } else if !set_has("@*#?$!-", c) {
        return None;
    }
    Some(skip_continuations(s, k))
}

/// `PARAMETER_PROMPT_RE = /@(?:\\\n)*[EP](?:\\\n)*\}/y`
fn parameter_prompt(s: &[u16], i: isize) -> bool {
    if !js::is(at(s, i), b'@') {
        return false;
    }
    let k = skip_continuations(s, i + 1);
    js::set_has_opt("EP", at(s, k)) && js::is(at(s, skip_continuations(s, k + 1)), b'}')
}

/// `BASH_EXPANSION_RE = /\$(?:\\.|\\\n)*[({]|`/`
fn has_bash_expansion(s: &[u16]) -> bool {
    s.iter().enumerate().any(|(i, &c)| {
        if c == b'`' as u16 {
            return true;
        }
        if c != b'$' as u16 {
            return false;
        }
        let mut k = i + 1;
        while k + 1 < s.len() && s[k] == b'\\' as u16 && (js::is_dot(s[k + 1]) || s[k + 1] == b'\n' as u16) {
            k += 2;
        }
        k < s.len() && set_has("({", s[k])
    })
}

/// `BASH_ESCAPE_RE = /\\(?:x[\da-fA-F]{1,2}|[0-7]{1,3}|u[\da-fA-F]{1,4}|U[\da-fA-F]{1,8})/`
fn has_bash_escape(s: &[u16]) -> bool {
    s.windows(2).enumerate().any(|(i, pair)| {
        pair[0] == b'\\' as u16 && {
            let c = pair[1];
            (b'0' as u16..=b'7' as u16).contains(&c)
                || (set_has("xuU", c) && s.get(i + 2).is_some_and(|&d| js::is_hex(d)))
        }
    })
}

/// `/[A-Za-z_]|(?:\$(?![?#!-]|\$(?!\[)))/` on arithmetic text: it names a variable.
fn arithmetic_names_variable(s: &[u16]) -> bool {
    s.iter().enumerate().any(|(i, &c)| {
        js::is_ident_start(c)
            || (c == b'$' as u16 && {
                let next = s.get(i + 1).copied();
                !js::set_has_opt("?#!-", next) && !(next == Some(b'$' as u16) && s.get(i + 2) != Some(&(b'[' as u16)))
            })
    })
}

/// `/^(?:-[A-Za-z]*v)?(?:(?:dis_)?(?:functions|[gs]?aliases)|options|commands)(?:\+?=|\[|$)/`: a Zsh special
/// parameter whose value names code.
fn zsh_dynamic_name(w: &[u16]) -> bool {
    let rest = |p: usize| {
        ["dis_functions", "dis_aliases", "dis_galiases", "dis_saliases", "functions", "aliases", "galiases", "saliases", "options", "commands"]
            .iter()
            .any(|name| {
                starts_with_ascii(w, name, p as isize) && {
                    let e = p + name.len();
                    e == w.len()
                        || w[e] == b'=' as u16
                        || w[e] == b'[' as u16
                        || (w[e] == b'+' as u16 && w.get(e + 1) == Some(&(b'=' as u16)))
                }
            })
    };
    if w.first() == Some(&(b'-' as u16)) {
        let run_end = 1 + w[1..].iter().take_while(|&&c| js::is_alpha(c)).count();
        if (1..run_end).rev().any(|p| w[p] == b'v' as u16 && rest(p + 1)) {
            return true;
        }
    }
    rest(0)
}

/// `/^(?:(?:dis_)?(?:functions|[gs]?aliases)|options|commands)$/`
fn zsh_dynamic_parameter(name: &[u16]) -> bool {
    ["dis_functions", "dis_aliases", "dis_galiases", "dis_saliases", "functions", "aliases", "galiases", "saliases", "options", "commands"]
        .iter()
        .any(|item| js::eq_ascii(name, item))
}

/// `/^(?:no)?(?:globsubst|kshglob|promptsubst|promptvars|aliases)$/` on
/// `word.replace(/^[-+][A-Za-z0-9]*?o/i, "").toLowerCase().replace(/[_-]/g, "")`.
fn zsh_evaluating_option(w: &[u16]) -> bool {
    let mut start = 0;
    if w.first().is_some_and(|&c| c == b'-' as u16 || c == b'+' as u16) {
        let run_end = 1 + w[1..].iter().take_while(|&&c| js::is_alnum(c)).count();
        if let Some(o) = (1..run_end).find(|&p| w[p] == b'o' as u16 || w[p] == b'O' as u16) {
            start = o + 1;
        }
    }
    let lowered: JsString =
        js::to_lower(&w[start..]).into_iter().filter(|&c| c != b'_' as u16 && c != b'-' as u16).collect();
    let options = ["globsubst", "kshglob", "promptsubst", "promptvars", "aliases"];
    let bare = |s: &[u16]| options.iter().any(|option| js::eq_ascii(s, option));
    bare(&lowered) || (starts_with_ascii(&lowered, "no", 0) && bare(&lowered[2..]))
}

/// `/^[-+][A-Za-z0-9]*X/`: a signed option cluster holding `X`.
fn signed_flag_has(w: &[u16], letter: u8) -> bool {
    w.first().is_some_and(|&c| c == b'-' as u16 || c == b'+' as u16) && alnum_run_has(&w[1..], letter)
}

/// `/^-[A-Za-z0-9]*X/`
fn dash_alnum_has(w: &[u16], letter: u8) -> bool {
    w.first() == Some(&(b'-' as u16)) && alnum_run_has(&w[1..], letter)
}

fn alnum_run_has(s: &[u16], letter: u8) -> bool {
    s.iter().take_while(|&&c| js::is_alnum(c)).any(|&c| c == letter as u16)
}

/// `/^-[A-Za-z]*X/`
fn dash_letters_then(w: &[u16], letter: u8) -> bool {
    w.first() == Some(&(b'-' as u16)) && w[1..].iter().take_while(|&&c| js::is_alpha(c)).any(|&c| c == letter as u16)
}

/// `/^-[nf]*p/`, or with `exact` `/^-[nf]*p$/`.
fn wait_flag(w: &[u16], exact: bool) -> bool {
    if w.first() != Some(&(b'-' as u16)) {
        return false;
    }
    let run = 1 + w[1..].iter().take_while(|&&c| c == b'n' as u16 || c == b'f' as u16).count();
    w.get(run) == Some(&(b'p' as u16)) && (!exact || run + 1 == w.len())
}

/// `/^-[cl]*a$/`
fn exec_argv0_flag(w: &[u16]) -> bool {
    w.len() >= 2
        && w[0] == b'-' as u16
        && w[w.len() - 1] == b'a' as u16
        && w[1..w.len() - 1].iter().all(|&c| c == b'c' as u16 || c == b'l' as u16)
}

/// `/\+?=(?:\(|[$`])/`: Bash re-parses the assigned value.
fn assignment_reparses(w: &[u16]) -> bool {
    w.windows(2).any(|pair| pair[0] == b'=' as u16 && set_has("($`", pair[1]))
}

/// `/[#"\n]|\\'/`
fn dash_unsafe_tail(s: &[u16]) -> bool {
    s.iter().any(|&c| set_has("#\"\n", c)) || s.windows(2).any(|p| p[0] == b'\\' as u16 && p[1] == b'\'' as u16)
}

/// `/^(?:\d+-?|-)$/`: a duplicated descriptor target, not a file.
fn is_fd_target(s: &[u16]) -> bool {
    if js::eq_ascii(s, "-") {
        return true;
    }
    let digits = s.iter().take_while(|&&c| js::is_digit(c)).count();
    digits > 0 && (digits == s.len() || (digits + 1 == s.len() && s[digits] == b'-' as u16))
}

/// `/^(?:\d+|\{[A-Za-z_][A-Za-z0-9_]*\})$/`: a word naming the descriptor of the redirect after it.
fn is_fd_prefix(s: &[u16]) -> bool {
    (!s.is_empty() && s.iter().all(|&c| js::is_digit(c)))
        || (s.len() >= 3 && s[0] == b'{' as u16 && s[s.len() - 1] == b'}' as u16 && is_name(&s[1..s.len() - 1]))
}

/// `/^[A-Za-z_][A-Za-z0-9_]*$/`
fn is_name(s: &[u16]) -> bool {
    s.first().is_some_and(|&c| js::is_ident_start(c)) && s[1..].iter().all(|&c| js::is_word(c))
}

/// `/^[A-Za-z_][A-Za-z0-9_]*\+?$/`
fn is_assignment_head(s: &[u16]) -> bool {
    is_name(s) || (s.last() == Some(&(b'+' as u16)) && is_name(&s[..s.len() - 1]))
}

/// `/[A-Za-z_0-9@*#?$!-]/` on the character after `$`.
fn is_parameter_char(c: u16) -> bool {
    js::is_word(c) || set_has("@*#?$!-", c)
}
