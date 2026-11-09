//! The PowerShell half of `scan.ts` (`scanPowerShell` and every `powerShell*` helper), function for function.
//! The closures of `scanPowerShellList` are the methods of `List`.

use std::collections::HashSet;

use super::{Command, MAX_INPUT_LENGTH, MAX_SUBSTITUTION_DEPTH, Reason, Scan};
use crate::js::{self, JsString, at, len, set_has, slice, starts_with_ascii};

type Res<T> = Result<T, Reason>;
type Skipped = HashSet<isize>;

fn skipped_has(skipped: Option<&Skipped>, index: isize) -> bool {
    skipped.is_some_and(|set| set.contains(&index))
}

pub fn scan_powershell(input: &[u16]) -> Scan {
    if len(input) > MAX_INPUT_LENGTH {
        return Scan::Opaque(Reason::InvalidStructure);
    }
    // PowerShell's Unicode quotes, dashes, and whitespace differ from JavaScript's token rules.
    if input.iter().any(|&c| matches!(c, 0 | 0x85 | 0x2013..=0x2015 | 0x2018..=0x201e | 0xfeff)) {
        return Scan::Opaque(Reason::InvalidStructure);
    }
    let mut budget = (MAX_INPUT_LENGTH * MAX_SUBSTITUTION_DEPTH) as i64;
    match scan_powershell_list(input, 0, 0, &mut budget, None, Hash::False, None) {
        Err(reason) => Scan::Opaque(reason),
        Ok((commands, _)) => Scan::Scanned(commands),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hash {
    False,
    True,
    Clause,
    Switch,
}

// The TS clause names, one of which is "clause".
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Member,
    Block,
    If,
    Try,
    Clause,
    Switch,
    Paren,
    Do,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Do,
    If,
    Try,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Indexable {
    False,
    Variable,
    Closed,
    Member,
}

struct List<'i> {
    input: &'i [u16],
    commands: Vec<Command>,
    nested_commands: Vec<Command>,
    words: Vec<JsString>,
    raw_words: Vec<JsString>,
    word_ends: Vec<isize>,
    initial_expression: bool,
    initial_clause: Option<Clause>,
    segment: isize,
    word: JsString,
    started: bool,
    word_start: isize,
    expression: bool,
    clause: Option<Clause>,
    after: Option<After>,
    indexable: Indexable,
    member_chain_start: Option<isize>,
    has_member_access: bool,
    has_file_redirect: bool,
    stop_parsing: bool,
    command_end: isize,
    statement_head: bool,
    invalid: bool,
    redirect_target: bool,
    dangling: bool,
    invocation: bool,
}

impl List<'_> {
    fn previous_word(&self) -> &[u16] {
        if self.started { &self.word } else { self.words.last().map_or(&[], |w| w.as_slice()) }
    }

    fn after_member_operator(&self) -> bool {
        self.indexable == Indexable::Member && self.previous_word().last().is_some_and(|&c| set_has(".:", c))
    }

    fn reset_member(&mut self) {
        self.indexable = Indexable::False;
        self.member_chain_start = None;
        self.has_member_access = false;
    }

    fn finish_word(&mut self, end: isize) {
        if !self.started {
            return;
        }
        // Generic tokens can spell stop-parsing with escapes or embedded quotes; literal strings cannot.
        if js::eq_ascii(&self.word, "--%")
            && !self.words.is_empty()
            && !self.expression
            && self.clause.is_none()
            && !self.redirect_target
            && !at(self.input, self.word_start).is_some_and(|c| set_has("'\"", c))
            && !self.raw_words.last().is_some_and(|w| w.last() == Some(&(b',' as u16)))
            && !parameter_colon(self.raw_words.last().map_or(&[], |w| w.as_slice()))
        {
            self.stop_parsing = true;
        }
        if !self.redirect_target {
            if self.words.is_empty() && !self.invocation {
                self.segment = self.word_start;
            }
            self.words.push(std::mem::take(&mut self.word));
            self.raw_words.push(slice(self.input, self.word_start, end).to_vec());
            self.word_ends.push(end);
            self.dangling = false;
        }
        self.command_end = end;
        self.redirect_target = false;
        self.word.clear();
        self.started = false;
    }

    fn finish_command(&mut self, end: isize, required: bool) {
        self.finish_word(end);
        if !self.words.is_empty() && ((!self.expression && self.clause.is_none()) || self.has_file_redirect) {
            let segment = self.segment;
            self.commands.push(Command {
                resource: js::trim_end(slice(self.input, segment, self.command_end)).to_vec(),
                words: self.words.clone(),
                raw_words: self.raw_words.clone(),
                word_ends: Some(self.word_ends.iter().map(|value| value - segment).collect()),
                statement_head: self.statement_head && !self.invocation && !self.expression && self.clause.is_none(),
                ..Command::default()
            });
        } else if (!self.expression && self.clause.is_none() && self.invocation)
            || (required && self.words.is_empty() && self.nested_commands.is_empty())
            || self.clause.is_some_and(|clause| clause != Clause::Member)
        {
            self.invalid = true;
        }
        if self.redirect_target {
            self.invalid = true;
        }
        let nested = std::mem::take(&mut self.nested_commands);
        self.commands.extend(nested);
        self.words.clear();
        self.raw_words.clear();
        self.word_ends.clear();
        self.redirect_target = false;
        self.has_file_redirect = false;
        self.invocation = false;
        self.expression = self.initial_expression;
        self.clause = self.initial_clause;
        self.after = None;
        self.reset_member();
        self.stop_parsing = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_powershell_list(
    input: &[u16],
    start: isize,
    depth: isize,
    budget: &mut i64,
    close: Option<u16>,
    hash: Hash,
    skipped: Option<&Skipped>,
) -> Res<(Vec<Command>, isize)> {
    if depth >= MAX_SUBSTITUTION_DEPTH || *budget < 0 {
        return Err(Reason::InvalidStructure);
    }
    let initial_expression = hash == Hash::True || hash == Hash::Clause;
    let initial_clause = if hash == Hash::Clause || hash == Hash::Switch { Some(Clause::Member) } else { None };
    let mut l = List {
        input,
        commands: Vec::new(),
        nested_commands: Vec::new(),
        words: Vec::new(),
        raw_words: Vec::new(),
        word_ends: Vec::new(),
        initial_expression,
        initial_clause,
        segment: start,
        word: JsString::new(),
        started: false,
        word_start: start,
        expression: initial_expression,
        clause: initial_clause,
        after: None,
        indexable: Indexable::False,
        member_chain_start: None,
        has_member_access: false,
        has_file_redirect: false,
        stop_parsing: false,
        command_end: start,
        statement_head: true,
        invalid: false,
        redirect_target: false,
        dangling: false,
        invocation: false,
    };
    let mut quote: Option<u16> = None;
    let mut standalone = false;
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        *budget -= 1;
        if *budget < 0 {
            return Err(Reason::InvalidStructure);
        }
        if skipped_has(skipped, index) {
            continue;
        }
        let c = input[index as usize];
        let next = at(input, index + 1);
        let in_expression = l.expression && !l.redirect_target;
        let at_boundary = !l.started || (!in_expression && l.word.last() == Some(&(b',' as u16)));
        if !l.started {
            l.word_start = index;
        }
        if l.stop_parsing {
            let stop = power_shell_stop_parsing(input, index, skipped);
            let region = slice(input, index, stop);
            let text = js::trim(region);
            if !text.is_empty() {
                l.word_start = index + region.iter().position(|&c| !js::is_space(c)).unwrap() as isize;
                l.word = text.to_vec();
                l.started = true;
                l.finish_word(l.word_start + len(text));
            }
            l.stop_parsing = false;
            index = stop - 1;
            continue;
        }
        if let Some(q) = quote {
            let mut next_quote = index + 1;
            while skipped_has(skipped, next_quote) {
                next_quote += 1;
            }
            if c == q && at(input, next_quote) == Some(q) {
                l.word.push(q);
                index = next_quote;
            } else if c == q {
                quote = None;
                if standalone {
                    l.finish_word(index + 1);
                    l.indexable = Indexable::Closed;
                }
            } else if q == b'"' as u16 && c == b'`' as u16 {
                let Some(escape) = power_shell_escape(input, index, false, skipped) else {
                    return Err(Reason::UnterminatedEscape);
                };
                l.word.extend_from_slice(&escape.value);
                index = escape.end;
            } else if q == b'"' as u16 && c == b'$' as u16 && js::is(next, b'{') {
                let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                    return Err(Reason::InvalidStructure);
                };
                l.word.extend_from_slice(slice(input, index, end + 1));
                index = end;
            } else if q == b'"' as u16 && c == b'$' as u16 && js::is(next, b'(') {
                let Some(sub) = power_shell_sub_expression(input, index + 2, false, budget, skipped) else {
                    return Err(Reason::InvalidStructure);
                };
                let (commands, result_end) =
                    scan_powershell_list(input, index + 2, depth + 1, budget, Some(b')' as u16), Hash::False, Some(&sub.skipped))?;
                l.nested_commands.extend(commands);
                let end = match sub.raw_end {
                    Some(raw_end) if raw_end > result_end => raw_end,
                    _ => result_end,
                };
                l.word.extend_from_slice(slice(input, index, end + 1));
                index = end;
            } else {
                l.word.push(c);
            }
            continue;
        }
        if c == b'\'' as u16 || c == b'"' as u16 {
            if !l.started && l.words.is_empty() && !l.invocation {
                l.expression = true;
            }
            quote = Some(c);
            let dynamic_member = l.after_member_operator();
            standalone = at_boundary || (l.expression && !l.redirect_target) || dynamic_member;
            if dynamic_member {
                l.has_member_access = true;
            } else if standalone {
                l.member_chain_start = Some(index);
                l.has_member_access = false;
            } else {
                l.reset_member();
            }
            l.started = true;
            continue;
        }
        if c == b'`' as u16 {
            let Some(escape) = power_shell_escape(input, index, at_boundary || in_expression, skipped) else {
                return Err(Reason::UnterminatedEscape);
            };
            let raw_space = js::is_space_opt(next);
            if (in_expression || at_boundary) && l.started && raw_space {
                l.finish_word(index);
            } else if !at_boundary || !raw_space {
                // At a token boundary escaped whitespace is trivia, not a new argument.
                l.started = true;
                if !(l.indexable == Indexable::Member && js::any_code_point(&escape.value, word_class)) {
                    l.reset_member();
                }
                l.word.extend_from_slice(&escape.value);
            }
            index = escape.end;
            continue;
        }
        if !l.started
            && !l.words.is_empty()
            && !l.expression
            && l.clause.is_none()
            && !l.redirect_target
            && stop_parsing_token(input, index)
        {
            l.word = js::units("--%");
            l.started = true;
            l.finish_word(index + 3);
            index += 2;
            continue;
        }
        if c == b'<' as u16 && js::is(next, b'#') && (at_boundary || in_expression || l.indexable == Indexable::Member) {
            let member_operator = l.after_member_operator();
            if l.started {
                l.finish_word(index);
            }
            let end = js::index_of_seq(input, &js::units("#>"), index + 2);
            if end < 0 {
                return Err(Reason::InvalidStructure);
            }
            if !member_operator {
                l.reset_member();
            }
            index = end + 1;
            continue;
        }
        if c == b'#' as u16
            && (at_boundary
                || l.indexable == Indexable::Member
                || (in_expression && !(close == Some(b']' as u16) && type_name(&l.word))))
        {
            let prev_comma = l.previous_word().last() == Some(&(b',' as u16));
            let member_operator = l.after_member_operator();
            if l.clause.is_some_and(|clause| clause != Clause::Member) || prev_comma || member_operator {
                l.finish_word(index);
            } else if !l.words.is_empty() || l.started || l.invocation {
                l.finish_command(index, false);
            }
            l.statement_head = !l.dangling;
            let endings = [js::index_of(input, b'\n' as u16, index), js::index_of(input, b'\r' as u16, index)];
            let Some(newline) = endings.into_iter().filter(|&ending| ending >= 0).min() else {
                index = n;
                break;
            };
            index = if input[newline as usize] == b'\r' as u16 && js::is(at(input, newline + 1), b'\n') {
                newline + 1
            } else {
                newline
            };
            continue;
        }
        if in_expression && l.started && (c == b'>' as u16 || (c == b'*' as u16 && js::is(next, b'>'))) {
            l.finish_word(index);
        }
        let redirect = if !l.started
            && (c == b'>' as u16
                || c == b'*' as u16
                || (js::is_digit(c) && (!l.words.is_empty() || merge_redirect(input, index))))
        {
            power_shell_redirect(input, index)
        } else {
            Redirect::None
        };
        if redirect == Redirect::Invalid {
            return Err(Reason::InvalidRedirect);
        }
        if let Redirect::Valid { length, ampersand } = redirect {
            if l.redirect_target || l.words.is_empty() || l.clause.is_some_and(|clause| clause != Clause::Member) {
                return Err(Reason::InvalidRedirect);
            }
            l.redirect_target = !ampersand;
            if l.redirect_target {
                l.has_file_redirect = true;
            }
            l.reset_member();
            index += length - 1;
            l.command_end = index + 1;
            continue;
        }
        if !l.started
            && l.words.is_empty()
            && !l.invocation
            && !l.expression
            && l.clause.is_none()
            && c == b':' as u16
            && let Some(label) = label(input, index)
        {
            index += label - 1;
            continue;
        }
        if !l.started
            && !l.invocation
            && l.clause.is_none()
            && (if l.words.is_empty() {
                !l.expression
            } else {
                l.words.iter().all(|item| starts_with_ascii(item, "[", 0))
            })
            && js::is_alpha(c)
            && let Some(keyword) = keyword(input, index)
        {
            let is = |name: &str| js::eq_ascii(&keyword, name);
            let accepted = if !l.words.is_empty() {
                (is("param") && param_open(input, index)) || is("class") || is("enum")
            } else {
                (is("param") && param_open(input, index))
                    || (is("foreach") && foreach_open(input, index))
                    || (is("until") && l.after == Some(After::Do))
                    || ((is("else") || is("elseif")) && l.after == Some(After::If))
                    || ((is("catch") || is("finally")) && l.after == Some(After::Try))
                    || [
                        "if", "for", "while", "do", "switch", "function", "filter", "workflow", "parallel", "sequence",
                        "configuration", "try", "begin", "process", "end", "clean", "trap", "class", "enum", "data",
                        "dynamicparam", "using", "break", "continue",
                    ]
                    .iter()
                    .any(|name| is(name))
            };
            if accepted {
                l.expression = ["class", "enum", "break", "continue"].iter().any(|name| is(name));
                l.clause = Some(if is("using") || is("break") || is("continue") {
                    Clause::Member
                } else if is("param") || is("until") || (is("while") && l.after == Some(After::Do)) {
                    Clause::Paren
                } else if is("do") {
                    Clause::Do
                } else if is("if") || is("elseif") {
                    Clause::If
                } else if is("try") || is("catch") {
                    Clause::Try
                } else if is("switch") {
                    Clause::Switch
                } else if is("class") || is("enum") {
                    Clause::Clause
                } else {
                    Clause::Block
                });
                l.after = None;
                index += len(&keyword) - 1;
                continue;
            }
            if l.words.is_empty() && (is("return") || is("throw") || is("exit")) {
                l.after = None;
                index += len(&keyword) - 1;
                continue;
            }
        }
        let assign = in_expression
            && l.clause.is_none()
            && (c == b'=' as u16 || (set_has("-+*/%", c) && js::is(next, b'=')));
        if assign || (in_expression && close == Some(b')' as u16) && !l.started && in_keyword(input, index)) {
            if assign {
                l.finish_word(index);
            }
            l.words.clear();
            l.raw_words.clear();
            l.word_ends.clear();
            l.expression = false;
            l.has_file_redirect = false;
            l.reset_member();
            if !assign || c != b'=' as u16 {
                index += 1;
            }
            continue;
        }
        if at_boundary && c == b'@' as u16 && !at_opener(input, index + 1) {
            return Err(Reason::InvalidStructure);
        }
        if at_boundary && c == b'@' as u16 && (js::is(next, b'\'') || js::is(next, b'"')) {
            if l.started {
                l.finish_word(index);
            }
            l.word_start = index;
            let literal = power_shell_here_string(input, index, depth, budget, skipped)?;
            if l.words.is_empty() && !l.invocation {
                l.expression = true;
            }
            l.nested_commands.extend(literal.commands);
            l.word = literal.source;
            l.started = true;
            index = literal.end;
            l.finish_word(index + 1);
            l.member_chain_start = Some(l.word_start);
            l.has_member_access = false;
            l.indexable = Indexable::Closed;
            continue;
        }
        if c == b'$' as u16 && js::is(next, b'{') {
            let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                return Err(Reason::InvalidStructure);
            };
            if !l.started && l.words.is_empty() && !l.invocation && hash != Hash::Switch {
                l.expression = true;
            }
            let dynamic_member = l.after_member_operator();
            if dynamic_member {
                l.indexable = Indexable::Closed;
                l.has_member_access = true;
            } else if at_boundary {
                l.member_chain_start = Some(index);
                l.has_member_access = false;
                l.indexable = Indexable::Closed;
            } else {
                l.reset_member();
            }
            l.word.extend_from_slice(slice(input, index, end + 1));
            l.started = true;
            index = end;
            if l.expression && !l.redirect_target && !js::is(at(input, index + 1), b'[') {
                l.finish_word(index + 1);
            }
            continue;
        }
        let opener = if ((c == b'$' as u16 || (at_boundary && c == b'@' as u16)) && js::is(next, b'('))
            || (at_boundary && c == b'@' as u16 && js::is(next, b'{'))
        {
            Some(index + 1)
        } else if c == b'(' as u16
            || c == b'{' as u16
            || (c == b'[' as u16
                && (in_expression
                    || l.indexable != Indexable::False
                    || (!l.started
                        && !l.invocation
                        && (l.words.is_empty() || matches!(l.clause, Some(Clause::Block | Clause::Try))))))
        {
            Some(index)
        } else {
            None
        };
        if let Some(opener) = opener {
            let token_index = index;
            let open_char = input[opener as usize];
            let dynamic_member_opener = l.after_member_operator();
            let prior_indexable = l.indexable != Indexable::False;
            let is_method_call = c == b'(' as u16
                && opener == token_index
                && l.member_chain_start.is_some()
                && ((l.indexable == Indexable::Member && l.started && !l.word.last().is_some_and(|&c| set_has(".:", c)))
                    || (l.indexable == Indexable::Closed && !l.started && l.has_member_access));
            let standalone_block = at_boundary || in_expression || c == b'(' as u16 || open_char == b'{' as u16;
            let block_close = match open_char {
                0x28 => b')',
                0x5b => b']',
                _ => b'}',
            } as u16;
            let block_hash = if open_char == b'[' as u16 || (c == b'@' as u16 && open_char == b'{' as u16) {
                Hash::True
            } else if l.clause == Some(Clause::Switch) && open_char == b'{' as u16 {
                Hash::Switch
            } else if (l.clause == Some(Clause::Clause) && open_char == b'{' as u16)
                || (close == Some(b']' as u16) && c == b'(' as u16 && l.started)
            {
                Hash::Clause
            } else {
                Hash::False
            };
            if l.started && (c == b'{' as u16 || c == b'(' as u16 || (c == b'@' as u16 && at_boundary)) {
                l.finish_word(index);
                if l.stop_parsing {
                    index -= 1;
                    continue;
                }
                l.word_start = index;
            }
            if !l.started && l.words.is_empty() && !l.invocation {
                l.expression = true;
            }
            let (commands, result_end) =
                scan_powershell_list(input, opener + 1, depth + 1, budget, Some(block_close), block_hash, skipped)?;
            l.nested_commands.extend(commands);
            l.started = true;
            l.word.extend_from_slice(slice(input, index, result_end + 1));
            index = result_end;
            if is_method_call && let Some(chain_start) = l.member_chain_start {
                l.finish_word(index + 1);
                let call_resource = slice(input, chain_start, result_end + 1).to_vec();
                let head = slice(input, chain_start, opener).to_vec();
                let args = slice(input, opener, result_end + 1).to_vec();
                let word_ends = vec![len(&head), len(&call_resource)];
                l.nested_commands.push(Command {
                    resource: call_resource,
                    words: vec![head.clone(), args.clone()],
                    raw_words: vec![head, args],
                    word_ends: Some(word_ends),
                    ..Command::default()
                });
                l.indexable = Indexable::Closed;
                l.has_member_access = false;
            } else if open_char == b'[' as u16 {
                l.indexable = Indexable::Closed;
                if !prior_indexable {
                    l.member_chain_start = Some(token_index);
                }
                if !js::is(at(input, index + 1), b'[') {
                    l.finish_word(index + 1);
                }
            } else if standalone_block {
                l.finish_word(index + 1);
                l.indexable = Indexable::Closed;
                if !dynamic_member_opener {
                    l.member_chain_start = Some(token_index);
                }
                l.has_member_access = dynamic_member_opener;
            }
            if (l.clause == Some(Clause::Paren) && c == b'(' as u16)
                || (l.clause.is_some_and(|clause| clause != Clause::Paren) && c == b'{' as u16)
            {
                let completed = match l.clause {
                    Some(Clause::Do) => Some(After::Do),
                    Some(Clause::If) => Some(After::If),
                    Some(Clause::Try) => Some(After::Try),
                    _ => None,
                };
                l.clause = Some(Clause::Member);
                l.finish_command(index + 1, false);
                l.after = completed;
            }
            continue;
        }
        if Some(c) == close {
            break;
        }
        if c == b'}' as u16 || c == b')' as u16 || (c == b']' as u16 && in_expression) {
            return Err(Reason::InvalidStructure);
        }
        if !l.started
            && l.words.is_empty()
            && ((c == b'&' as u16 && !js::is(next, b'&'))
                || (c == b'.' as u16 && next.is_none_or(|next| js::is_space(next) || set_has("\"'$(),;{|}&", next))))
        {
            if l.invocation {
                return Err(Reason::InvalidStructure);
            }
            l.segment = index;
            l.invocation = true;
            l.after = None;
            continue;
        }
        if js::is_space(c) && c != b'\n' as u16 && c != b'\r' as u16 {
            let member_operator = l.after_member_operator();
            l.finish_word(index);
            if !member_operator {
                l.reset_member();
            }
            continue;
        }
        let separator: Option<&str> = if c == b'\r' as u16 && js::is(next, b'\n') {
            Some("\r\n")
        } else if c == b'&' as u16 && js::is(next, b'&') {
            Some("&&")
        } else if c == b'|' as u16 && js::is(next, b'|') {
            Some("||")
        } else if set_has(";|&\n\r", c) {
            Some(match c {
                0x3b => ";",
                0x7c => "|",
                0x26 => "&",
                0x0a => "\n",
                _ => "\r",
            })
        } else {
            None
        };
        if let Some(separator) = separator {
            if close == Some(b']' as u16) && [";", "|", "||", "&&", "&"].contains(&separator) {
                return Err(Reason::InvalidStructure);
            }
            let step = separator.len() as isize - 1;
            if ["\n", "\r", "\r\n"].contains(&separator) {
                let prev_comma = l.previous_word().last() == Some(&(b',' as u16));
                let member_operator = l.after_member_operator();
                if l.clause.is_some_and(|clause| clause != Clause::Member) || prev_comma || member_operator {
                    l.finish_word(index);
                    index += step;
                    if !member_operator {
                        l.reset_member();
                    }
                    continue;
                }
                if !l.started && l.words.is_empty() && !l.invocation {
                    index += step;
                    l.reset_member();
                    continue;
                }
            }
            let required = l.dangling || ![";", "\n", "\r", "\r\n"].contains(&separator);
            l.finish_command(index, required);
            l.dangling = ![";", "&", "\n", "\r", "\r\n"].contains(&separator);
            l.statement_head = !l.dangling;
            index += step;
            continue;
        }
        if !l.started && l.words.is_empty() && !l.invocation && hash != Hash::Switch && expression_start(input, index) {
            l.expression = true;
        }
        let next_space_or_expression = |at_index: isize| !js::is_space_opt(at(input, at_index)) || in_expression;
        if (at_boundary || l.after_member_operator()) && c == b'$' as u16 {
            if indexable_variable(input, index + 1) {
                if l.after_member_operator() {
                    l.has_member_access = true;
                } else {
                    l.member_chain_start = Some(index);
                    l.has_member_access = false;
                }
                l.indexable = Indexable::Variable;
            } else {
                l.reset_member();
            }
        } else if l.indexable == Indexable::Closed {
            l.indexable = if (c == b'.' as u16 && !js::is(next, b'.') && next_space_or_expression(index + 1))
                || (!l.started && c == b':' as u16 && js::is(next, b':'))
                || (!l.started
                    && c == b'?' as u16
                    && js::is(next, b'.')
                    && !js::is(at(input, index + 2), b'.')
                    && next_space_or_expression(index + 2))
            {
                Indexable::Member
            } else if !l.started && c == b'?' as u16 && js::is(next, b'[') {
                Indexable::Closed
            } else {
                Indexable::False
            };
            if l.indexable == Indexable::Member {
                l.has_member_access = true;
            } else if l.indexable == Indexable::False {
                l.reset_member();
            }
        } else if l.indexable == Indexable::Variable {
            l.indexable = if (c == b'.' as u16 && !js::is(next, b'.') && next_space_or_expression(index + 1))
                || (c == b':' as u16 && js::is(next, b':'))
            {
                Indexable::Member
            } else if (c == b'.' as u16 && js::is(next, b'.')) || !variable_class(c as u32) {
                Indexable::False
            } else {
                Indexable::Variable
            };
            if l.indexable == Indexable::Member {
                l.has_member_access = true;
            } else if l.indexable == Indexable::False {
                l.reset_member();
            }
        } else if l.indexable == Indexable::Member {
            l.indexable = if c == b'.' as u16 && js::is(next, b'.') {
                Indexable::False
            } else if (c == b'.' as u16 && next_space_or_expression(index + 1))
                || (c == b':' as u16 && (js::is(next, b':') || js::is(at(input, index - 1), b':')))
                || (c == b'?' as u16 && js::is(next, b'.') && !js::is(at(input, index + 2), b'.'))
                || word_class(c as u32)
            {
                Indexable::Member
            } else {
                Indexable::False
            };
            if l.indexable == Indexable::False {
                l.reset_member();
            }
        }
        l.after = None;
        l.started = true;
        l.dangling = false;
        l.word.push(c);
    }

    if close.is_some() && at(input, index) != close {
        return Err(Reason::InvalidStructure);
    }
    if quote.is_some() {
        return Err(Reason::UnterminatedQuote);
    }
    l.finish_command(index, false);
    if l.redirect_target || l.invalid || l.dangling {
        return Err(Reason::InvalidStructure);
    }
    if l.commands.iter().any(|command| command.words.first().is_none_or(|w| w.is_empty())) {
        return Err(Reason::DynamicCommandName);
    }
    Ok((l.commands, index))
}

/// `[\p{L}\p{Nl}\d_]` (u flag) on one code point.
fn word_class(c: u32) -> bool {
    js::is_letter(c) || (b'0' as u32..=b'9' as u32).contains(&c) || c == b'_' as u32
}

/// `[\p{L}\p{Nl}\d_:?]` (u flag).
fn variable_class(c: u32) -> bool {
    word_class(c) || c == b':' as u32 || c == b'?' as u32
}

struct Escape {
    value: JsString,
    end: isize,
}

fn power_shell_escape(input: &[u16], start: isize, line_continuation: bool, skipped: Option<&Skipped>) -> Option<Escape> {
    let mut next = start + 1;
    while skipped_has(skipped, next) {
        next += 1;
    }
    let c = at(input, next)?;
    if c == b'\r' as u16 || c == b'\n' as u16 {
        let crlf = line_continuation && c == b'\r' as u16 && js::is(at(input, next + 1), b'\n');
        return Some(Escape {
            value: if crlf { js::units("\r\n") } else { vec![c] },
            end: next + if crlf { 1 } else { 0 },
        });
    }
    if c == b'u' as u16 && js::is(at(input, next + 1), b'{') {
        // POWERSHELL_UNICODE_ESCAPE_RE = /u\{([0-9a-f]{1,6})\}/iy
        let digits = input[(next + 2) as usize..].iter().take(7).take_while(|&&d| js::is_hex(d)).count() as isize;
        if digits == 0 || digits > 6 || !js::is(at(input, next + 2 + digits), b'}') {
            return None;
        }
        let point = slice(input, next + 2, next + 2 + digits)
            .iter()
            .fold(0u32, |acc, &d| acc * 16 + char::from(d as u8).to_digit(16).unwrap());
        if point > 0x10ffff {
            return None;
        }
        let mut value = JsString::new();
        js::push_code_point(&mut value, point);
        return Some(Escape { value, end: next + digits + 2 });
    }
    let value = match u8::try_from(c) {
        Ok(b'0') => 0,
        Ok(b'a') => 0x07,
        Ok(b'b') => 0x08,
        Ok(b'e') => 0x1b,
        Ok(b'f') => 0x0c,
        Ok(b'n') => 0x0a,
        Ok(b'r') => 0x0d,
        Ok(b't') => 0x09,
        Ok(b'v') => 0x0b,
        _ => c,
    };
    Some(Escape { value: vec![value], end: next })
}

fn power_shell_braced_variable(input: &[u16], start: isize, skipped: Option<&Skipped>) -> Option<isize> {
    let n = len(input);
    let mut index = start + 1;
    loop {
        index += 1;
        if index >= n {
            return None;
        }
        if skipped_has(skipped, index) {
            continue;
        }
        let c = input[index as usize];
        if c == b'`' as u16 {
            index = power_shell_escape(input, index, false, skipped)?.end;
            continue;
        }
        if c == b'}' as u16 {
            return if index > start + 2 { Some(index) } else { None };
        }
    }
}

struct SubExpression {
    skipped: Skipped,
    raw_end: Option<isize>,
}

fn power_shell_sub_expression(
    input: &[u16],
    start: isize,
    here_string: bool,
    budget: &mut i64,
    skipped: Option<&Skipped>,
) -> Option<SubExpression> {
    let mut next_skipped = skipped.cloned().unwrap_or_default();
    let mut depth = 1;
    let mut raw_end = None;
    let n = len(input);
    let mut index = start - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        *budget -= 1;
        if *budget < 0 {
            return None;
        }
        if next_skipped.contains(&index) {
            continue;
        }
        let c = input[index as usize];
        if c == b'(' as u16 {
            depth += 1;
        } else if c == b')' as u16 {
            depth -= 1;
            if depth == 0 && raw_end.is_none() {
                raw_end = Some(index);
            }
        } else if !here_string && (c == b'"' as u16 || c == b'`' as u16) {
            let mut next = index + 1;
            while next_skipped.contains(&next) {
                next += 1;
            }
            if js::is(at(input, next), b'"') {
                next_skipped.insert(index);
                index = next;
            }
        }
    }
    Some(SubExpression { skipped: next_skipped, raw_end })
}

fn power_shell_stop_parsing(input: &[u16], start: isize, skipped: Option<&Skipped>) -> isize {
    let mut quoted = false;
    let n = len(input);
    for index in start.max(0)..n {
        if skipped_has(skipped, index) {
            continue;
        }
        let c = input[index as usize];
        if c == b'"' as u16 {
            quoted = !quoted;
        }
        if c == b'\r' as u16
            || c == b'\n' as u16
            || ((c == b'|' as u16 || (c == b'&' as u16 && js::is(at(input, index + 1), b'&'))) && !quoted)
        {
            return index;
        }
    }
    n
}

struct HereString {
    commands: Vec<Command>,
    source: JsString,
    end: isize,
}

fn power_shell_here_string(
    input: &[u16],
    start: isize,
    depth: isize,
    budget: &mut i64,
    skipped: Option<&Skipped>,
) -> Res<HereString> {
    let Some(header) = here_header(input, start) else {
        return Err(Reason::UnterminatedQuote);
    };
    let quote = input[(start + 1) as usize];
    let body = start + header;
    let mut commands = Vec::new();
    let mut line_start = true;
    let n = len(input);
    let mut index = body - 1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        *budget -= 1;
        if *budget < 0 {
            return Err(Reason::InvalidStructure);
        }
        if skipped_has(skipped, index) {
            continue;
        }
        let c = input[index as usize];
        if line_start && c == quote && js::is(at(input, index + 1), b'@') {
            let end = if index > body && js::is(at(input, index - 1), b'\n') && js::is(at(input, index - 2), b'\r') {
                index - 2
            } else if index > body {
                index - 1
            } else {
                index
            };
            return Ok(HereString { commands, source: slice(input, body, end).to_vec(), end: index + 1 });
        }
        if quote == b'"' as u16 {
            if c == b'`' as u16 {
                let Some(escape) = power_shell_escape(input, index, false, skipped) else {
                    return Err(Reason::UnterminatedEscape);
                };
                index = escape.end;
                line_start = false;
                continue;
            }
            if c == b'$' as u16 && js::is(at(input, index + 1), b'{') {
                let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                    return Err(Reason::InvalidStructure);
                };
                index = end;
                line_start = false;
                continue;
            }
            if c == b'$' as u16 && js::is(at(input, index + 1), b'(') {
                let Some(sub) = power_shell_sub_expression(input, index + 2, true, budget, skipped) else {
                    return Err(Reason::InvalidStructure);
                };
                let (nested, result_end) =
                    scan_powershell_list(input, index + 2, depth + 1, budget, Some(b')' as u16), Hash::False, Some(&sub.skipped))?;
                commands.extend(nested);
                index = match sub.raw_end {
                    Some(raw_end) if raw_end > result_end => raw_end,
                    _ => result_end,
                };
                line_start = false;
                continue;
            }
        }
        line_start = c == b'\r' as u16 || c == b'\n' as u16;
    }
    Err(Reason::UnterminatedQuote)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redirect {
    None,
    Invalid,
    Valid { length: isize, ampersand: bool },
}

fn power_shell_redirect(input: &[u16], index: isize) -> Redirect {
    let mut cursor = index;
    if js::is(at(input, cursor), b'*') {
        cursor += 1;
    } else {
        while js::is_digit_opt(at(input, cursor)) {
            cursor += 1;
        }
    }
    if !js::is(at(input, cursor), b'>') {
        return Redirect::None;
    }
    cursor += 1;
    if js::is(at(input, cursor), b'>') {
        cursor += 1;
    }
    if js::is(at(input, cursor), b'&') {
        cursor += 1;
        while js::is_digit_opt(at(input, cursor)) {
            cursor += 1;
        }
    }
    let redirect = slice(input, index, cursor);
    // /^(?:(?:[1-6]|\*)?>>?|[2-6*]>&1)$/
    let stream = |c: u16, low: u8| (low as u16..=b'6' as u16).contains(&c) || c == b'*' as u16;
    let valid = match redirect {
        [g] | [g, _] if *g == b'>' as u16 => redirect.iter().all(|&c| c == b'>' as u16),
        [s, rest @ ..] if stream(*s, b'1') && (js::eq_ascii(rest, ">") || js::eq_ascii(rest, ">>")) => true,
        [s, rest @ ..] if stream(*s, b'2') && js::eq_ascii(rest, ">&1") => true,
        _ => false,
    };
    if valid {
        Redirect::Valid { length: len(redirect), ampersand: js::includes(redirect, b'&' as u16) }
    } else {
        Redirect::Invalid
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The regular expressions of the PowerShell scanner.

/// A token boundary of PowerShell keywords: `(?=$|[\s&(),;{|}])`.
fn keyword_boundary(s: &[u16], i: isize) -> bool {
    i == len(s) || at(s, i).is_some_and(|c| js::is_space(c) || set_has("&(),;{|}", c))
}

/// `POWERSHELL_STOP_PARSING_RE = /--%(?=$|[\s;|&(){}])/y`
fn stop_parsing_token(s: &[u16], i: isize) -> bool {
    starts_with_ascii(s, "--%", i) && (i + 3 == len(s) || at(s, i + 3).is_some_and(|c| js::is_space(c) || set_has(";|&(){}", c)))
}

/// `POWERSHELL_PARAMETER_COLON_RE = /^-[\p{L}\p{Nl}_?][\p{L}\p{Nl}\d_?-]*:$/u`
fn parameter_colon(s: &[u16]) -> bool {
    if s.first() != Some(&(b'-' as u16)) || s.last() != Some(&(b':' as u16)) || s.len() < 3 {
        return false;
    }
    let body = &s[1..s.len() - 1];
    let mut i = 0;
    let mut first = true;
    while let Some((c, n)) = js::code_point(body, i) {
        let ok = js::is_letter(c)
            || c == b'_' as u32
            || c == b'?' as u32
            || (!first && ((b'0' as u32..=b'9' as u32).contains(&c) || c == b'-' as u32));
        if !ok {
            return false;
        }
        first = false;
        i += n;
    }
    !first
}

/// `POWERSHELL_MERGE_REDIRECT_RE = /\d>&/y`
fn merge_redirect(s: &[u16], i: isize) -> bool {
    js::is_digit_opt(at(s, i)) && js::is(at(s, i + 1), b'>') && js::is(at(s, i + 2), b'&')
}

/// `POWERSHELL_LABEL_RE = /:[A-Za-z_]\w*(?=\s*(?:while|for|foreach|do|switch)(?=$|[\s&(),;{|}]))/iy`: the match
/// length. The label shrinks as the regex backtracks until a loop keyword follows.
fn label(s: &[u16], i: isize) -> Option<isize> {
    if !js::is(at(s, i), b':') || !at(s, i + 1).is_some_and(js::is_ident_start) {
        return None;
    }
    let mut run_end = i + 2;
    while at(s, run_end).is_some_and(js::is_word) {
        run_end += 1;
    }
    (i + 2..=run_end).rev().find_map(|end| {
        let mut k = end;
        while js::is_space_opt(at(s, k)) {
            k += 1;
        }
        ["while", "for", "foreach", "do", "switch"]
            .iter()
            .any(|keyword| js::starts_with_ignore_case(s, k, keyword) && keyword_boundary(s, k + keyword.len() as isize))
            .then_some(end - i)
    })
}

/// `POWERSHELL_KEYWORD_RE = /[A-Za-z]+(?=$|[\s&(),;{|}])/y`, lowercased.
fn keyword(s: &[u16], i: isize) -> Option<JsString> {
    let mut end = i;
    while at(s, end).is_some_and(js::is_alpha) {
        end += 1;
    }
    (end > i && keyword_boundary(s, end)).then(|| js::to_lower(slice(s, i, end)))
}

/// `/<word>\s*\(/isy`
fn word_then_paren(s: &[u16], i: isize, word: &str) -> bool {
    if !js::starts_with_ignore_case(s, i, word) {
        return false;
    }
    let mut k = i + word.len() as isize;
    while js::is_space_opt(at(s, k)) {
        k += 1;
    }
    js::is(at(s, k), b'(')
}

/// `POWERSHELL_FOREACH_RE = /foreach\s*\(/isy`
fn foreach_open(s: &[u16], i: isize) -> bool {
    word_then_paren(s, i, "foreach")
}

/// `POWERSHELL_PARAM_RE = /param\s*\(/isy`
fn param_open(s: &[u16], i: isize) -> bool {
    word_then_paren(s, i, "param")
}

/// `POWERSHELL_IN_RE = /in(?=$|[\s&(),;{|}])/iy`
fn in_keyword(s: &[u16], i: isize) -> bool {
    js::starts_with_ignore_case(s, i, "in") && keyword_boundary(s, i + 2)
}

/// `POWERSHELL_AT_OPENER_RE = /[({'"\p{L}\p{Nl}\d_?]/uy`
fn at_opener(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    js::code_point(s, js::sticky_unicode_start(s, i)).is_some_and(|(c, _)| {
        js::is_letter(c) || (c < 128 && (js::is_digit(c as u16) || set_has("({'\"_?", c as u16)))
    })
}

/// `POWERSHELL_INDEXABLE_VAR_RE = /(?::[\p{L}\p{Nl}\d_?]+|[\p{L}\p{Nl}\d_][\p{L}\p{Nl}\d_?]*)/uy`
fn indexable_variable(s: &[u16], i: isize) -> bool {
    if i > len(s) {
        return false;
    }
    let start = js::sticky_unicode_start(s, i);
    match js::code_point(s, start) {
        Some((c, n)) if c == b':' as u32 => {
            js::code_point(s, start + n).is_some_and(|(c, _)| word_class(c) || c == b'?' as u32)
        }
        Some((c, _)) => word_class(c),
        None => false,
    }
}

/// `POWERSHELL_HERE_HEADER_RE = /@['"][^\S\r\n]*(?:\r\n|\r|\n)/y`: the match length.
fn here_header(s: &[u16], i: isize) -> Option<isize> {
    if !js::is(at(s, i), b'@') || !js::set_has_opt("'\"", at(s, i + 1)) {
        return None;
    }
    let mut k = i + 2;
    while at(s, k).is_some_and(|c| js::is_space(c) && c != b'\r' as u16 && c != b'\n' as u16) {
        k += 1;
    }
    if js::is(at(s, k), b'\r') && js::is(at(s, k + 1), b'\n') {
        Some(k + 2 - i)
    } else if js::set_has_opt("\r\n", at(s, k)) {
        Some(k + 1 - i)
    } else {
        None
    }
}

/// `/^[\p{L}\p{Nl}_][\p{L}\p{Nl}\d_.#+`\\]*$/u`: a type name, where `#` stays part of the word.
fn type_name(s: &[u16]) -> bool {
    let mut i = 0;
    let mut first = true;
    while let Some((c, n)) = js::code_point(s, i) {
        let ok = js::is_letter(c)
            || c == b'_' as u32
            || (!first && (word_class(c) || (c < 128 && set_has(".#+`\\", c as u16))));
        if !ok {
            return false;
        }
        first = false;
        i += n;
    }
    !first
}

/// `[\p{L}\p{Nl}]` under the `iu` flags at the code point starting at `i`.
fn letter_fold_at(s: &[u16], i: isize) -> bool {
    js::code_point(s, i).is_some_and(|(c, _)| js::is_letter_fold(c))
}

/// `POWERSHELL_EXPRESSION_RE` (flags `iuy`): whether an expression starts at `i`. Only the outcome matters, so each
/// alternative is a predicate; the few places where the regex backtracks are noted.
fn expression_start(s: &[u16], i: isize) -> bool {
    let c = match at(s, i) {
        Some(c) => c,
        None => return false,
    };
    let next = at(s, i + 1);
    let next_in = |extra: &str| {
        next.is_some_and(|n| js::is_digit(n) || set_has(extra, n)) || letter_fold_at(s, i + 1)
    };
    variable_expression(s, i)
        // !(?![\p{L}\p{Nl}_.\d])
        || (c == b'!' as u16 && !next_in("_."))
        // \+(?![=\p{L}\p{Nl}_.\d])
        || (c == b'+' as u16 && !next_in("=_."))
        // --(?!=)
        || (c == b'-' as u16 && js::is(next, b'-') && !js::is(at(s, i + 2), b'='))
        // -(?![=\p{L}\p{Nl}\d_?.])
        || (c == b'-' as u16 && !next_in("=_?."))
        // ,(?!\s*$)
        || (c == b',' as u16 && !s[(i + 1) as usize..].iter().all(|&c| js::is_space(c)))
        || number_expression(s, i)
        // -(?:not|bnot|join|[ic]?split)(?![\p{L}\p{Nl}])
        || (c == b'-' as u16
            && ["not", "bnot", "join", "split", "isplit", "csplit"].iter().any(|word| {
                js::starts_with_fold(s, i + 1, word) && !letter_fold_at(s, i + 1 + word.len() as isize)
            }))
}

/// `!*(?:\+\+|--|[+-])?\$(?:\{|(?:\?|\$|\^)(?=$|[\s;|&(){},#>])|:[\p{L}\p{Nl}\d_?]+|[\p{L}\p{Nl}\d_]...)`. The sign
/// options consume different lengths before a `$`, so at most one leaves a `$` next.
fn variable_expression(s: &[u16], i: isize) -> bool {
    let mut p = i;
    while js::is(at(s, p), b'!') {
        p += 1;
    }
    let dollar = if (starts_with_ascii(s, "++", p) || starts_with_ascii(s, "--", p)) && js::is(at(s, p + 2), b'$') {
        p + 2
    } else if js::set_has_opt("+-", at(s, p)) && js::is(at(s, p + 1), b'$') {
        p + 1
    } else {
        p
    };
    if !js::is(at(s, dollar), b'$') {
        return false;
    }
    let r = dollar + 1;
    let class = |c: u32| js::is_letter_fold(c) || (b'0' as u32..=b'9' as u32).contains(&c) || c == b'_' as u32;
    match at(s, r) {
        Some(c) if c == b'{' as u16 => true,
        Some(c) if set_has("?$^", c) => {
            r + 1 == len(s) || at(s, r + 1).is_some_and(|n| js::is_space(n) || set_has(";|&(){},#>", n))
        }
        Some(c) if c == b':' as u16 => js::code_point(s, r + 1).is_some_and(|(c, _)| class(c) || c == b'?' as u32),
        Some(_) => js::code_point(s, r).is_some_and(|(c, _)| class(c)),
        None => false,
    }
}

/// The number alternative of `POWERSHELL_EXPRESSION_RE`:
/// `!*[+-]?(?:0[xX][\da-fA-F]+|0[bB][01]+|(?:\d+(?:\.(?!\.)\d*|(?!\.(?!\.)))|\.\d+)(?:[eE][+-]?\d+)?)` then the
/// suffixes and the delimiter lookahead. A hex literal may give back a trailing `d` to the type suffix; no other
/// backtracking can succeed.
fn number_expression(s: &[u16], i: isize) -> bool {
    let mut j = i;
    while js::is(at(s, j), b'!') {
        j += 1;
    }
    if js::set_has_opt("+-", at(s, j)) {
        j += 1;
    }
    if js::is(at(s, j), b'0') && js::set_has_opt("xX", at(s, j + 1)) && at(s, j + 2).is_some_and(js::is_hex) {
        let mut k = j + 2;
        while at(s, k).is_some_and(js::is_hex) {
            k += 1;
        }
        if number_suffix(s, k) || (k - 1 > j + 2 && js::set_has_opt("dD", at(s, k - 1)) && number_suffix(s, k - 1)) {
            return true;
        }
    }
    if js::is(at(s, j), b'0') && js::set_has_opt("bB", at(s, j + 1)) && js::set_has_opt("01", at(s, j + 2)) {
        let mut k = j + 2;
        while js::set_has_opt("01", at(s, k)) {
            k += 1;
        }
        if number_suffix(s, k) {
            return true;
        }
    }
    let mut k = if js::is_digit_opt(at(s, j)) {
        let mut k = j;
        while js::is_digit_opt(at(s, k)) {
            k += 1;
        }
        if js::is(at(s, k), b'.') && !js::is(at(s, k + 1), b'.') {
            k += 1;
            while js::is_digit_opt(at(s, k)) {
                k += 1;
            }
        }
        k
    } else if js::is(at(s, j), b'.') && js::is_digit_opt(at(s, j + 1)) {
        let mut k = j + 1;
        while js::is_digit_opt(at(s, k)) {
            k += 1;
        }
        k
    } else {
        return false;
    };
    if js::set_has_opt("eE", at(s, k)) {
        let mut m = k + 1;
        if js::set_has_opt("+-", at(s, m)) {
            m += 1;
        }
        if js::is_digit_opt(at(s, m)) {
            while js::is_digit_opt(at(s, m)) {
                m += 1;
            }
            k = m;
        }
    }
    number_suffix(s, k)
}

/// `(?:[dDlLnN]|[uU][sSyYlL]?|[sSyY])?(?:[kKmMgGtTpP][bB])?(?=$|[\s!#%&()*+,\-/.;<=>\]|{}])` (flags `iu`).
fn number_suffix(s: &[u16], k: isize) -> bool {
    let fold_in = |c: Option<u16>, letters: &str| c.is_some_and(|c| letters.bytes().any(|b| js::eq_fold(c, b)));
    let mut ends = Vec::with_capacity(3);
    let c = at(s, k);
    if fold_in(c, "dln") {
        ends.push(k + 1);
    }
    if fold_in(c, "u") {
        if fold_in(at(s, k + 1), "syl") {
            ends.push(k + 2);
        }
        ends.push(k + 1);
    }
    if fold_in(c, "sy") {
        ends.push(k + 1);
    }
    ends.push(k);
    let delimiter = |p: isize| p == len(s) || at(s, p).is_some_and(|c| js::is_space(c) || set_has("!#%&()*+,-/.;<=>]|{}", c));
    ends.into_iter()
        .any(|p| (fold_in(at(s, p), "kmgtp") && fold_in(at(s, p + 1), "b") && delimiter(p + 2)) || delimiter(p))
}
