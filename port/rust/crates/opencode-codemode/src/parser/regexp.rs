//! acorn's regular expression validator (`regexp.js`, `unicode-property-data.js`), for `ecmaVersion: "latest"`.

use super::chars::{is_identifier_char, is_identifier_start};
use super::state::{Parser, R};
use std::collections::HashMap;

const CHAR_SET_NONE: u8 = 0;
const CHAR_SET_OK: u8 = 1;
const CHAR_SET_STRING: u8 = 2;

/// A failed check: the reason, prefixed with the pattern when reported.
pub(super) struct RegexErr(&'static str);

type RR<T> = Result<T, RegexErr>;

#[derive(Clone, Copy)]
struct Branch {
    parent: Option<usize>,
    base: usize,
}

pub(super) struct RegExpValidationState<'p> {
    pub start: usize,
    source: &'p [u16],
    pub flags: Vec<u16>,
    switch_u: bool,
    switch_v: bool,
    switch_n: bool,
    pos: usize,
    last_int_value: f64,
    last_string_value: String,
    last_assertion_is_quantifiable: bool,
    num_capturing_parens: i64,
    max_back_reference: f64,
    group_names: HashMap<String, Vec<usize>>,
    back_reference_names: Vec<String>,
    branches: Vec<Branch>,
    branch_id: Option<usize>,
}

impl<'p> RegExpValidationState<'p> {
    pub(super) fn new(start: usize, pattern: &'p [u16], flags: &[u16]) -> RegExpValidationState<'p> {
        let unicode_sets = flags.contains(&118);
        let unicode = flags.contains(&117);
        let (switch_u, switch_v, switch_n) = if unicode_sets { (true, true, true) } else { (unicode, false, unicode) };
        RegExpValidationState {
            start,
            source: pattern,
            flags: flags.to_vec(),
            switch_u,
            switch_v,
            switch_n,
            pos: 0,
            last_int_value: 0.0,
            last_string_value: String::new(),
            last_assertion_is_quantifiable: false,
            num_capturing_parens: 0,
            max_back_reference: 0.0,
            group_names: HashMap::new(),
            back_reference_names: Vec::new(),
            branches: Vec::new(),
            branch_id: None,
        }
    }

    fn at(&self, i: usize, force_u: bool) -> i64 {
        let s = self.source;
        let l = s.len();
        if i >= l {
            return -1;
        }
        let c = s[i] as i64;
        if !(force_u || self.switch_u) || c <= 0xD7FF || c >= 0xE000 || i + 1 >= l {
            return c;
        }
        let next = s[i + 1] as i64;
        if (0xDC00..=0xDFFF).contains(&next) { (c << 10) + next - 0x35FDC00 } else { c }
    }

    fn next_index(&self, i: usize, force_u: bool) -> usize {
        let s = self.source;
        let l = s.len();
        if i >= l {
            return l;
        }
        let c = s[i] as i64;
        if !(force_u || self.switch_u) || c <= 0xD7FF || c >= 0xE000 || i + 1 >= l {
            return i + 1;
        }
        let next = s[i + 1] as i64;
        if !(0xDC00..=0xDFFF).contains(&next) {
            return i + 1;
        }
        i + 2
    }

    fn current(&self) -> i64 {
        self.at(self.pos, false)
    }
    fn current_u(&self, force_u: bool) -> i64 {
        self.at(self.pos, force_u)
    }
    fn lookahead(&self) -> i64 {
        self.at(self.next_index(self.pos, false), false)
    }
    fn advance(&mut self) {
        self.pos = self.next_index(self.pos, false);
    }
    fn advance_u(&mut self, force_u: bool) {
        self.pos = self.next_index(self.pos, force_u);
    }
    fn eat(&mut self, ch: i64) -> bool {
        if self.current() == ch {
            self.advance();
            return true;
        }
        false
    }
    fn eat_chars(&mut self, chs: &[i64]) -> bool {
        let mut pos = self.pos;
        for &ch in chs {
            let current = self.at(pos, false);
            if current == -1 || current != ch {
                return false;
            }
            pos = self.next_index(pos, false);
        }
        self.pos = pos;
        true
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-Pattern
    fn pattern(&mut self) -> RR<()> {
        self.pos = 0;
        self.last_int_value = 0.0;
        self.last_string_value.clear();
        self.last_assertion_is_quantifiable = false;
        self.num_capturing_parens = 0;
        self.max_back_reference = 0.0;
        self.group_names.clear();
        self.back_reference_names.clear();
        self.branch_id = None;

        self.disjunction()?;

        if self.pos != self.source.len() {
            // Make the same messages as V8.
            if self.eat(0x29) {
                return Err(RegexErr("Unmatched ')'"));
            }
            if self.eat(0x5D) || self.eat(0x7D) {
                return Err(RegexErr("Lone quantifier brackets"));
            }
        }
        if self.max_back_reference > self.num_capturing_parens as f64 {
            return Err(RegexErr("Invalid escape"));
        }
        for name in &self.back_reference_names {
            if !self.group_names.contains_key(name) {
                return Err(RegexErr("Invalid named capture referenced"));
            }
        }
        Ok(())
    }

    fn new_branch(&mut self, parent: Option<usize>, base: Option<usize>) -> usize {
        let index = self.branches.len();
        self.branches.push(Branch { parent, base: base.unwrap_or(index) });
        index
    }

    fn separated_from(&self, this: usize, alt: Option<usize>) -> bool {
        let mut me = Some(this);
        while let Some(m) = me {
            let mut other = alt;
            while let Some(o) = other {
                if self.branches[m].base == self.branches[o].base && m != o {
                    return true;
                }
                other = self.branches[o].parent;
            }
            me = self.branches[m].parent;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-Disjunction
    fn disjunction(&mut self) -> RR<()> {
        self.branch_id = Some(self.new_branch(self.branch_id, None));
        self.alternative()?;
        while self.eat(0x7C) {
            let cur = self.branches[self.branch_id.unwrap()];
            self.branch_id = Some(self.new_branch(cur.parent, Some(cur.base)));
            self.alternative()?;
        }
        self.branch_id = self.branches[self.branch_id.unwrap()].parent;

        // Make the same message as V8.
        if self.eat_quantifier(true)? {
            return Err(RegexErr("Nothing to repeat"));
        }
        if self.eat(0x7B) {
            return Err(RegexErr("Lone quantifier brackets"));
        }
        Ok(())
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-Alternative
    fn alternative(&mut self) -> RR<()> {
        while self.pos < self.source.len() && self.eat_term()? {}
        Ok(())
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-Term
    fn eat_term(&mut self) -> RR<bool> {
        if self.eat_assertion()? {
            // Handle `QuantifiableAssertion Quantifier` alternative.
            if self.last_assertion_is_quantifiable && self.eat_quantifier(false)? {
                // Make the same message as V8.
                if self.switch_u {
                    return Err(RegexErr("Invalid quantifier"));
                }
            }
            return Ok(true);
        }
        let atom = if self.switch_u { self.eat_atom()? } else { self.eat_extended_atom()? };
        if atom {
            self.eat_quantifier(false)?;
            return Ok(true);
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-Assertion
    fn eat_assertion(&mut self) -> RR<bool> {
        let start = self.pos;
        self.last_assertion_is_quantifiable = false;

        // ^, $
        if self.eat(0x5E) || self.eat(0x24) {
            return Ok(true);
        }

        // \b \B
        if self.eat(0x5C) {
            if self.eat(0x42) || self.eat(0x62) {
                return Ok(true);
            }
            self.pos = start;
        }

        // Lookahead / Lookbehind
        if self.eat(0x28) && self.eat(0x3F) {
            let lookbehind = self.eat(0x3C);
            if self.eat(0x3D) || self.eat(0x21) {
                self.disjunction()?;
                if !self.eat(0x29) {
                    return Err(RegexErr("Unterminated group"));
                }
                self.last_assertion_is_quantifiable = !lookbehind;
                return Ok(true);
            }
        }

        self.pos = start;
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-Quantifier
    fn eat_quantifier(&mut self, no_error: bool) -> RR<bool> {
        if self.eat_quantifier_prefix(no_error)? {
            self.eat(0x3F);
            return Ok(true);
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-QuantifierPrefix
    fn eat_quantifier_prefix(&mut self, no_error: bool) -> RR<bool> {
        Ok(self.eat(0x2A) || self.eat(0x2B) || self.eat(0x3F) || self.eat_braced_quantifier(no_error)?)
    }

    fn eat_braced_quantifier(&mut self, no_error: bool) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x7B) {
            let mut max = -1.0;
            if self.eat_decimal_digits() {
                let min = self.last_int_value;
                if self.eat(0x2C) && self.eat_decimal_digits() {
                    max = self.last_int_value;
                }
                if self.eat(0x7D) {
                    // SyntaxError in https://www.ecma-international.org/ecma-262/8.0/#sec-term
                    if max != -1.0 && max < min && !no_error {
                        return Err(RegexErr("numbers out of order in {} quantifier"));
                    }
                    return Ok(true);
                }
            }
            if self.switch_u && !no_error {
                return Err(RegexErr("Incomplete quantifier"));
            }
            self.pos = start;
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-Atom
    fn eat_atom(&mut self) -> RR<bool> {
        Ok(self.eat_pattern_characters()
            || self.eat(0x2E)
            || self.eat_reverse_solidus_atom_escape()?
            || self.eat_character_class()?
            || self.eat_uncapturing_group()?
            || self.eat_capturing_group()?)
    }

    fn eat_reverse_solidus_atom_escape(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x5C) {
            if self.eat_atom_escape()? {
                return Ok(true);
            }
            self.pos = start;
        }
        Ok(false)
    }

    fn eat_uncapturing_group(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x28) {
            if self.eat(0x3F) {
                let add_modifiers = self.eat_modifiers();
                let has_hyphen = self.eat(0x2D);
                if !add_modifiers.is_empty() || has_hyphen {
                    for (i, modifier) in add_modifiers.iter().enumerate() {
                        if add_modifiers[i + 1..].contains(modifier) {
                            return Err(RegexErr("Duplicate regular expression modifiers"));
                        }
                    }
                    if has_hyphen {
                        let remove_modifiers = self.eat_modifiers();
                        if add_modifiers.is_empty() && remove_modifiers.is_empty() && self.current() == 0x3A {
                            return Err(RegexErr("Invalid regular expression modifiers"));
                        }
                        for (i, modifier) in remove_modifiers.iter().enumerate() {
                            if remove_modifiers[i + 1..].contains(modifier) || add_modifiers.contains(modifier) {
                                return Err(RegexErr("Duplicate regular expression modifiers"));
                            }
                        }
                    }
                }
                if self.eat(0x3A) {
                    self.disjunction()?;
                    if self.eat(0x29) {
                        return Ok(true);
                    }
                    return Err(RegexErr("Unterminated group"));
                }
            }
            self.pos = start;
        }
        Ok(false)
    }

    fn eat_capturing_group(&mut self) -> RR<bool> {
        if self.eat(0x28) {
            self.group_specifier()?;
            self.disjunction()?;
            if self.eat(0x29) {
                self.num_capturing_parens += 1;
                return Ok(true);
            }
            return Err(RegexErr("Unterminated group"));
        }
        Ok(false)
    }

    fn eat_modifiers(&mut self) -> Vec<i64> {
        let mut modifiers = Vec::new();
        loop {
            let ch = self.current();
            if ch == -1 || !(ch == 0x69 || ch == 0x6d || ch == 0x73) {
                break;
            }
            modifiers.push(ch);
            self.advance();
        }
        modifiers
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-ExtendedAtom
    fn eat_extended_atom(&mut self) -> RR<bool> {
        Ok(self.eat(0x2E)
            || self.eat_reverse_solidus_atom_escape()?
            || self.eat_character_class()?
            || self.eat_uncapturing_group()?
            || self.eat_capturing_group()?
            || self.eat_invalid_braced_quantifier()?
            || self.eat_extended_pattern_character())
    }

    fn eat_invalid_braced_quantifier(&mut self) -> RR<bool> {
        if self.eat_braced_quantifier(true)? {
            return Err(RegexErr("Nothing to repeat"));
        }
        Ok(false)
    }

    fn eat_syntax_character(&mut self) -> bool {
        let ch = self.current();
        if is_syntax_character(ch) {
            self.last_int_value = ch as f64;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-PatternCharacter (but eat eager)
    fn eat_pattern_characters(&mut self) -> bool {
        let start = self.pos;
        loop {
            let ch = self.current();
            if ch == -1 || is_syntax_character(ch) {
                break;
            }
            self.advance();
        }
        self.pos != start
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-ExtendedPatternCharacter
    fn eat_extended_pattern_character(&mut self) -> bool {
        let ch = self.current();
        if ch != -1
            && ch != 0x24
            && !(0x28..=0x2B).contains(&ch)
            && ch != 0x2E
            && ch != 0x3F
            && ch != 0x5B
            && ch != 0x5E
            && ch != 0x7C
        {
            self.advance();
            return true;
        }
        false
    }

    // GroupSpecifier :: [empty] | `?` GroupName
    fn group_specifier(&mut self) -> RR<()> {
        if self.eat(0x3F) {
            if !self.eat_group_name()? {
                return Err(RegexErr("Invalid group"));
            }
            let name = self.last_string_value.clone();
            if let Some(known) = self.group_names.get(&name) {
                for &alt in known {
                    if !self.separated_from(alt, self.branch_id) {
                        return Err(RegexErr("Duplicate capture group name"));
                    }
                }
            }
            let branch = self.branch_id;
            let entry = self.group_names.entry(name).or_default();
            entry.push(branch.unwrap());
        }
        Ok(())
    }

    // GroupName :: `<` RegExpIdentifierName `>`
    fn eat_group_name(&mut self) -> RR<bool> {
        self.last_string_value.clear();
        if self.eat(0x3C) {
            if self.eat_regexp_identifier_name()? && self.eat(0x3E) {
                return Ok(true);
            }
            return Err(RegexErr("Invalid capture group name"));
        }
        Ok(false)
    }

    fn eat_regexp_identifier_name(&mut self) -> RR<bool> {
        self.last_string_value.clear();
        if self.eat_regexp_identifier_start()? {
            push_code_point_string(&mut self.last_string_value, self.last_int_value as i64);
            while self.eat_regexp_identifier_part()? {
                push_code_point_string(&mut self.last_string_value, self.last_int_value as i64);
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn eat_regexp_identifier_start(&mut self) -> RR<bool> {
        let start = self.pos;
        let mut ch = self.current_u(true);
        self.advance_u(true);
        if ch == 0x5C && self.eat_regexp_unicode_escape_sequence(true)? {
            ch = self.last_int_value as i64;
        }
        if is_identifier_start(ch) || ch == 0x24 || ch == 0x5F {
            self.last_int_value = ch as f64;
            return Ok(true);
        }
        self.pos = start;
        Ok(false)
    }

    fn eat_regexp_identifier_part(&mut self) -> RR<bool> {
        let start = self.pos;
        let mut ch = self.current_u(true);
        self.advance_u(true);
        if ch == 0x5C && self.eat_regexp_unicode_escape_sequence(true)? {
            ch = self.last_int_value as i64;
        }
        if is_identifier_char(ch) || ch == 0x24 || ch == 0x5F || ch == 0x200C || ch == 0x200D {
            self.last_int_value = ch as f64;
            return Ok(true);
        }
        self.pos = start;
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-AtomEscape
    fn eat_atom_escape(&mut self) -> RR<bool> {
        if self.eat_back_reference()
            || self.eat_character_class_escape()? != CHAR_SET_NONE
            || self.eat_character_escape()?
            || (self.switch_n && self.eat_k_group_name()?)
        {
            return Ok(true);
        }
        if self.switch_u {
            // Make the same message as V8.
            if self.current() == 0x63 {
                return Err(RegexErr("Invalid unicode escape"));
            }
            return Err(RegexErr("Invalid escape"));
        }
        Ok(false)
    }

    fn eat_back_reference(&mut self) -> bool {
        let start = self.pos;
        if self.eat_decimal_escape() {
            let n = self.last_int_value;
            if self.switch_u {
                if n > self.max_back_reference {
                    self.max_back_reference = n;
                }
                return true;
            }
            if n <= self.num_capturing_parens as f64 {
                return true;
            }
            self.pos = start;
        }
        false
    }

    fn eat_k_group_name(&mut self) -> RR<bool> {
        if self.eat(0x6B) {
            if self.eat_group_name()? {
                self.back_reference_names.push(self.last_string_value.clone());
                return Ok(true);
            }
            return Err(RegexErr("Invalid named reference"));
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-CharacterEscape
    fn eat_character_escape(&mut self) -> RR<bool> {
        Ok(self.eat_control_escape()
            || self.eat_c_control_letter()
            || self.eat_zero()
            || self.eat_hex_escape_sequence()?
            || self.eat_regexp_unicode_escape_sequence(false)?
            || (!self.switch_u && self.eat_legacy_octal_escape_sequence())
            || self.eat_identity_escape())
    }

    fn eat_c_control_letter(&mut self) -> bool {
        let start = self.pos;
        if self.eat(0x63) {
            if self.eat_control_letter() {
                return true;
            }
            self.pos = start;
        }
        false
    }

    fn eat_zero(&mut self) -> bool {
        if self.current() == 0x30 && !is_decimal_digit(self.lookahead()) {
            self.last_int_value = 0.0;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-ControlEscape
    fn eat_control_escape(&mut self) -> bool {
        let value = match self.current() {
            0x74 => 9.0,
            0x6E => 10.0,
            0x76 => 11.0,
            0x66 => 12.0,
            0x72 => 13.0,
            _ => return false,
        };
        self.last_int_value = value;
        self.advance();
        true
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-ControlLetter
    fn eat_control_letter(&mut self) -> bool {
        let ch = self.current();
        if is_control_letter(ch) {
            self.last_int_value = (ch % 0x20) as f64;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-RegExpUnicodeEscapeSequence
    fn eat_regexp_unicode_escape_sequence(&mut self, force_u: bool) -> RR<bool> {
        let start = self.pos;
        let switch_u = force_u || self.switch_u;

        if self.eat(0x75) {
            if self.eat_fixed_hex_digits(4) {
                let lead = self.last_int_value;
                if switch_u && (55296.0..=56319.0).contains(&lead) {
                    let lead_surrogate_end = self.pos;
                    if self.eat(0x5C) && self.eat(0x75) && self.eat_fixed_hex_digits(4) {
                        let trail = self.last_int_value;
                        if (56320.0..=57343.0).contains(&trail) {
                            self.last_int_value = (lead - 55296.0) * 1024.0 + (trail - 56320.0) + 65536.0;
                            return Ok(true);
                        }
                    }
                    self.pos = lead_surrogate_end;
                    self.last_int_value = lead;
                }
                return Ok(true);
            }
            if switch_u
                && self.eat(0x7B)
                && self.eat_hex_digits()
                && self.eat(0x7D)
                && (0.0..=1114111.0).contains(&self.last_int_value)
            {
                return Ok(true);
            }
            if switch_u {
                return Err(RegexErr("Invalid unicode escape"));
            }
            self.pos = start;
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-IdentityEscape
    fn eat_identity_escape(&mut self) -> bool {
        if self.switch_u {
            if self.eat_syntax_character() {
                return true;
            }
            if self.eat(0x2F) {
                self.last_int_value = 47.0;
                return true;
            }
            return false;
        }
        let ch = self.current();
        if ch != 0x63 && (!self.switch_n || ch != 0x6B) {
            self.last_int_value = ch as f64;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-DecimalEscape
    fn eat_decimal_escape(&mut self) -> bool {
        self.last_int_value = 0.0;
        let mut ch = self.current();
        if (0x31..=0x39).contains(&ch) {
            loop {
                self.last_int_value = 10.0 * self.last_int_value + (ch - 0x30) as f64;
                self.advance();
                ch = self.current();
                if !(0x30..=0x39).contains(&ch) {
                    break;
                }
            }
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-CharacterClassEscape
    fn eat_character_class_escape(&mut self) -> RR<u8> {
        let ch = self.current();
        if is_character_class_escape(ch) {
            self.last_int_value = -1.0;
            self.advance();
            return Ok(CHAR_SET_OK);
        }
        let negate = ch == 0x50;
        if self.switch_u && (negate || ch == 0x70) {
            self.last_int_value = -1.0;
            self.advance();
            if self.eat(0x7B) {
                let result = self.eat_unicode_property_value_expression()?;
                if result != CHAR_SET_NONE && self.eat(0x7D) {
                    if negate && result == CHAR_SET_STRING {
                        return Err(RegexErr("Invalid property name"));
                    }
                    return Ok(result);
                }
            }
            return Err(RegexErr("Invalid property name"));
        }
        Ok(CHAR_SET_NONE)
    }

    // UnicodePropertyValueExpression :: UnicodePropertyName `=` UnicodePropertyValue | LoneUnicodePropertyNameOrValue
    fn eat_unicode_property_value_expression(&mut self) -> RR<u8> {
        let start = self.pos;
        if self.eat_unicode_property_name() && self.eat(0x3D) {
            let name = self.last_string_value.clone();
            if self.eat_unicode_property_value() {
                let value = self.last_string_value.clone();
                validate_unicode_property_name_and_value(&name, &value)?;
                return Ok(CHAR_SET_OK);
            }
        }
        self.pos = start;
        if self.eat_unicode_property_value() {
            let name_or_value = self.last_string_value.clone();
            return self.validate_unicode_property_name_or_value(&name_or_value);
        }
        Ok(CHAR_SET_NONE)
    }

    fn validate_unicode_property_name_or_value(&self, name_or_value: &str) -> RR<u8> {
        if word_in(BINARY_PROPERTIES, name_or_value) || word_in(GENERAL_CATEGORY_VALUES, name_or_value) {
            return Ok(CHAR_SET_OK);
        }
        if self.switch_v && word_in(BINARY_PROPERTIES_OF_STRINGS, name_or_value) {
            return Ok(CHAR_SET_STRING);
        }
        Err(RegexErr("Invalid property name"))
    }

    fn eat_unicode_property_name(&mut self) -> bool {
        self.last_string_value.clear();
        loop {
            let ch = self.current();
            if !(is_control_letter(ch) || ch == 0x5F) {
                break;
            }
            push_code_point_string(&mut self.last_string_value, ch);
            self.advance();
        }
        !self.last_string_value.is_empty()
    }

    fn eat_unicode_property_value(&mut self) -> bool {
        self.last_string_value.clear();
        loop {
            let ch = self.current();
            if !(is_control_letter(ch) || ch == 0x5F || is_decimal_digit(ch)) {
                break;
            }
            push_code_point_string(&mut self.last_string_value, ch);
            self.advance();
        }
        !self.last_string_value.is_empty()
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-CharacterClass
    fn eat_character_class(&mut self) -> RR<bool> {
        if self.eat(0x5B) {
            let negate = self.eat(0x5E);
            let result = self.class_contents()?;
            if !self.eat(0x5D) {
                return Err(RegexErr("Unterminated character class"));
            }
            if negate && result == CHAR_SET_STRING {
                return Err(RegexErr("Negated character class may contain strings"));
            }
            return Ok(true);
        }
        Ok(false)
    }

    // https://tc39.es/ecma262/#prod-ClassContents
    fn class_contents(&mut self) -> RR<u8> {
        if self.current() == 0x5D {
            return Ok(CHAR_SET_OK);
        }
        if self.switch_v {
            return self.class_set_expression();
        }
        self.non_empty_class_ranges()?;
        Ok(CHAR_SET_OK)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-NonemptyClassRanges
    fn non_empty_class_ranges(&mut self) -> RR<()> {
        while self.eat_class_atom()? {
            let left = self.last_int_value;
            if self.eat(0x2D) && self.eat_class_atom()? {
                let right = self.last_int_value;
                if self.switch_u && (left == -1.0 || right == -1.0) {
                    return Err(RegexErr("Invalid character class"));
                }
                if left != -1.0 && right != -1.0 && left > right {
                    return Err(RegexErr("Range out of order in character class"));
                }
            }
            if self.pos >= self.source.len() && self.current() == -1 {
                // acorn would spin here forever; the tokenizer never hands it such a pattern.
                break;
            }
        }
        Ok(())
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-ClassAtom
    fn eat_class_atom(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x5C) {
            if self.eat_class_escape()? {
                return Ok(true);
            }
            if self.switch_u {
                // Make the same message as V8.
                let ch = self.current();
                if ch == 0x63 || is_octal_digit(ch) {
                    return Err(RegexErr("Invalid class escape"));
                }
                return Err(RegexErr("Invalid escape"));
            }
            self.pos = start;
        }
        let ch = self.current();
        if ch != 0x5D {
            self.last_int_value = ch as f64;
            self.advance();
            return Ok(true);
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-ClassEscape
    fn eat_class_escape(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x62) {
            self.last_int_value = 8.0;
            return Ok(true);
        }
        if self.switch_u && self.eat(0x2D) {
            self.last_int_value = 45.0;
            return Ok(true);
        }
        if !self.switch_u && self.eat(0x63) {
            if self.eat_class_control_letter() {
                return Ok(true);
            }
            self.pos = start;
        }
        Ok(self.eat_character_class_escape()? != CHAR_SET_NONE || self.eat_character_escape()?)
    }

    // https://tc39.es/ecma262/#prod-ClassSetExpression
    fn class_set_expression(&mut self) -> RR<u8> {
        let mut result = CHAR_SET_OK;
        if self.eat_class_set_range()? {
        } else {
            let sub_result = self.eat_class_set_operand()?;
            if sub_result != CHAR_SET_NONE {
                if sub_result == CHAR_SET_STRING {
                    result = CHAR_SET_STRING;
                }
                // https://tc39.es/ecma262/#prod-ClassIntersection
                let start = self.pos;
                while self.eat_chars(&[0x26, 0x26]) {
                    if self.current() != 0x26 {
                        let sub_result = self.eat_class_set_operand()?;
                        if sub_result != CHAR_SET_NONE {
                            if sub_result != CHAR_SET_STRING {
                                result = CHAR_SET_OK;
                            }
                            continue;
                        }
                    }
                    return Err(RegexErr("Invalid character in character class"));
                }
                if start != self.pos {
                    return Ok(result);
                }
                // https://tc39.es/ecma262/#prod-ClassSubtraction
                while self.eat_chars(&[0x2D, 0x2D]) {
                    if self.eat_class_set_operand()? != CHAR_SET_NONE {
                        continue;
                    }
                    return Err(RegexErr("Invalid character in character class"));
                }
                if start != self.pos {
                    return Ok(result);
                }
            } else {
                return Err(RegexErr("Invalid character in character class"));
            }
        }
        // https://tc39.es/ecma262/#prod-ClassUnion
        loop {
            if self.eat_class_set_range()? {
                continue;
            }
            let sub_result = self.eat_class_set_operand()?;
            if sub_result == CHAR_SET_NONE {
                return Ok(result);
            }
            if sub_result == CHAR_SET_STRING {
                result = CHAR_SET_STRING;
            }
        }
    }

    // https://tc39.es/ecma262/#prod-ClassSetRange
    fn eat_class_set_range(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat_class_set_character()? {
            let left = self.last_int_value;
            if self.eat(0x2D) && self.eat_class_set_character()? {
                let right = self.last_int_value;
                if left != -1.0 && right != -1.0 && left > right {
                    return Err(RegexErr("Range out of order in character class"));
                }
                return Ok(true);
            }
            self.pos = start;
        }
        Ok(false)
    }

    // https://tc39.es/ecma262/#prod-ClassSetOperand
    fn eat_class_set_operand(&mut self) -> RR<u8> {
        if self.eat_class_set_character()? {
            return Ok(CHAR_SET_OK);
        }
        let result = self.eat_class_string_disjunction()?;
        if result != CHAR_SET_NONE {
            return Ok(result);
        }
        self.eat_nested_class()
    }

    // https://tc39.es/ecma262/#prod-NestedClass
    fn eat_nested_class(&mut self) -> RR<u8> {
        let start = self.pos;
        if self.eat(0x5B) {
            let negate = self.eat(0x5E);
            let result = self.class_contents()?;
            if self.eat(0x5D) {
                if negate && result == CHAR_SET_STRING {
                    return Err(RegexErr("Negated character class may contain strings"));
                }
                return Ok(result);
            }
            self.pos = start;
        }
        if self.eat(0x5C) {
            let result = self.eat_character_class_escape()?;
            if result != CHAR_SET_NONE {
                return Ok(result);
            }
            self.pos = start;
        }
        Ok(CHAR_SET_NONE)
    }

    // https://tc39.es/ecma262/#prod-ClassStringDisjunction
    fn eat_class_string_disjunction(&mut self) -> RR<u8> {
        let start = self.pos;
        if self.eat_chars(&[0x5C, 0x71]) {
            if self.eat(0x7B) {
                let result = self.class_string_disjunction_contents()?;
                if self.eat(0x7D) {
                    return Ok(result);
                }
            } else {
                // Make the same message as V8.
                return Err(RegexErr("Invalid escape"));
            }
            self.pos = start;
        }
        Ok(CHAR_SET_NONE)
    }

    fn class_string_disjunction_contents(&mut self) -> RR<u8> {
        let mut result = self.class_string()?;
        while self.eat(0x7C) {
            if self.class_string()? == CHAR_SET_STRING {
                result = CHAR_SET_STRING;
            }
        }
        Ok(result)
    }

    fn class_string(&mut self) -> RR<u8> {
        let mut count = 0;
        while self.eat_class_set_character()? {
            count += 1;
        }
        Ok(if count == 1 { CHAR_SET_OK } else { CHAR_SET_STRING })
    }

    // https://tc39.es/ecma262/#prod-ClassSetCharacter
    fn eat_class_set_character(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x5C) {
            if self.eat_character_escape()? || self.eat_class_set_reserved_punctuator() {
                return Ok(true);
            }
            if self.eat(0x62) {
                self.last_int_value = 8.0;
                return Ok(true);
            }
            self.pos = start;
            return Ok(false);
        }
        let ch = self.current();
        if ch < 0 || ch == self.lookahead() && is_class_set_reserved_double_punctuator_character(ch) {
            return Ok(false);
        }
        if is_class_set_syntax_character(ch) {
            return Ok(false);
        }
        self.advance();
        self.last_int_value = ch as f64;
        Ok(true)
    }

    fn eat_class_set_reserved_punctuator(&mut self) -> bool {
        let ch = self.current();
        if is_class_set_reserved_punctuator(ch) {
            self.last_int_value = ch as f64;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-ClassControlLetter
    fn eat_class_control_letter(&mut self) -> bool {
        let ch = self.current();
        if is_decimal_digit(ch) || ch == 0x5F {
            self.last_int_value = (ch % 0x20) as f64;
            self.advance();
            return true;
        }
        false
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-HexEscapeSequence
    fn eat_hex_escape_sequence(&mut self) -> RR<bool> {
        let start = self.pos;
        if self.eat(0x78) {
            if self.eat_fixed_hex_digits(2) {
                return Ok(true);
            }
            if self.switch_u {
                return Err(RegexErr("Invalid escape"));
            }
            self.pos = start;
        }
        Ok(false)
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-DecimalDigits
    fn eat_decimal_digits(&mut self) -> bool {
        let start = self.pos;
        self.last_int_value = 0.0;
        loop {
            let ch = self.current();
            if !is_decimal_digit(ch) {
                break;
            }
            self.last_int_value = 10.0 * self.last_int_value + (ch - 0x30) as f64;
            self.advance();
        }
        self.pos != start
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-HexDigits
    fn eat_hex_digits(&mut self) -> bool {
        let start = self.pos;
        self.last_int_value = 0.0;
        loop {
            let ch = self.current();
            if !is_hex_digit(ch) {
                break;
            }
            self.last_int_value = 16.0 * self.last_int_value + hex_to_int(ch) as f64;
            self.advance();
        }
        self.pos != start
    }

    // https://www.ecma-international.org/ecma-262/8.0/#prod-annexB-LegacyOctalEscapeSequence
    fn eat_legacy_octal_escape_sequence(&mut self) -> bool {
        if self.eat_octal_digit() {
            let n1 = self.last_int_value;
            if self.eat_octal_digit() {
                let n2 = self.last_int_value;
                if n1 <= 3.0 && self.eat_octal_digit() {
                    self.last_int_value += n1 * 64.0 + n2 * 8.0;
                } else {
                    self.last_int_value = n1 * 8.0 + n2;
                }
            } else {
                self.last_int_value = n1;
            }
            return true;
        }
        false
    }

    fn eat_octal_digit(&mut self) -> bool {
        let ch = self.current();
        if is_octal_digit(ch) {
            self.last_int_value = (ch - 0x30) as f64;
            self.advance();
            return true;
        }
        self.last_int_value = 0.0;
        false
    }

    fn eat_fixed_hex_digits(&mut self, length: usize) -> bool {
        let start = self.pos;
        self.last_int_value = 0.0;
        for _ in 0..length {
            let ch = self.current();
            if !is_hex_digit(ch) {
                self.pos = start;
                return false;
            }
            self.last_int_value = 16.0 * self.last_int_value + hex_to_int(ch) as f64;
            self.advance();
        }
        true
    }
}

impl<'a> Parser<'a> {
    /// Validate the flags part of a regular expression literal.
    pub(super) fn validate_regexp_flags(&self, state: &RegExpValidationState) -> R<()> {
        let flags = &state.flags;
        let mut u = false;
        let mut v = false;
        for (i, &flag) in flags.iter().enumerate() {
            if !b"gimuysdv".iter().any(|&f| f as u16 == flag) {
                return Err(self.raise(state.start, "Invalid regular expression flag"));
            }
            if flags[i + 1..].contains(&flag) {
                return Err(self.raise(state.start, "Duplicate regular expression flag"));
            }
            if flag == 117 {
                u = true;
            }
            if flag == 118 {
                v = true;
            }
        }
        if u && v {
            return Err(self.raise(state.start, "Invalid regular expression flag"));
        }
        Ok(())
    }

    /// Validate the pattern part of a regular expression literal.
    pub(super) fn validate_regexp_pattern(&self, state: &mut RegExpValidationState) -> R<()> {
        let mut result = state.pattern();
        // The goal symbol for the parse is |Pattern[~U, ~N]|. If the result of parsing contains a |GroupName|,
        // reparse with the goal symbol |Pattern[~U, +N]| and use this result instead.
        if result.is_ok() && !state.switch_n && !state.group_names.is_empty() {
            state.switch_n = true;
            result = state.pattern();
        }
        result.map_err(|RegexErr(message)| {
            let source = String::from_utf16_lossy(state.source);
            self.raise(state.start, format!("Invalid regular expression: /{source}/: {message}"))
        })
    }
}

fn push_code_point_string(out: &mut String, code: i64) {
    out.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
}

fn is_syntax_character(ch: i64) -> bool {
    ch == 0x24
        || (0x28..=0x2B).contains(&ch)
        || ch == 0x2E
        || ch == 0x3F
        || (0x5B..=0x5E).contains(&ch)
        || (0x7B..=0x7D).contains(&ch)
}

fn is_control_letter(ch: i64) -> bool {
    (0x41..=0x5A).contains(&ch) || (0x61..=0x7A).contains(&ch)
}

fn is_character_class_escape(ch: i64) -> bool {
    matches!(ch, 0x64 | 0x44 | 0x73 | 0x53 | 0x77 | 0x57)
}

fn is_decimal_digit(ch: i64) -> bool {
    (0x30..=0x39).contains(&ch)
}

fn is_hex_digit(ch: i64) -> bool {
    (0x30..=0x39).contains(&ch) || (0x41..=0x46).contains(&ch) || (0x61..=0x66).contains(&ch)
}

fn hex_to_int(ch: i64) -> i64 {
    if (0x41..=0x46).contains(&ch) {
        return 10 + (ch - 0x41);
    }
    if (0x61..=0x66).contains(&ch) {
        return 10 + (ch - 0x61);
    }
    ch - 0x30
}

fn is_octal_digit(ch: i64) -> bool {
    (0x30..=0x37).contains(&ch)
}

fn is_class_set_reserved_double_punctuator_character(ch: i64) -> bool {
    ch == 0x21
        || (0x23..=0x26).contains(&ch)
        || (0x2A..=0x2C).contains(&ch)
        || ch == 0x2E
        || (0x3A..=0x40).contains(&ch)
        || ch == 0x5E
        || ch == 0x60
        || ch == 0x7E
}

fn is_class_set_syntax_character(ch: i64) -> bool {
    ch == 0x28 || ch == 0x29 || ch == 0x2D || ch == 0x2F || (0x5B..=0x5D).contains(&ch) || (0x7B..=0x7D).contains(&ch)
}

fn is_class_set_reserved_punctuator(ch: i64) -> bool {
    matches!(ch, 0x21 | 0x23 | 0x25 | 0x26 | 0x2C | 0x2D | 0x3A..=0x3E | 0x40 | 0x60 | 0x7E)
}

fn word_in(list: &str, word: &str) -> bool {
    list.split(' ').any(|item| item == word)
}

fn validate_unicode_property_name_and_value(name: &str, value: &str) -> RR<()> {
    let values = match name {
        "General_Category" | "gc" => GENERAL_CATEGORY_VALUES,
        "Script" | "Script_Extensions" | "sc" | "scx" => SCRIPT_VALUES,
        _ => return Err(RegexErr("Invalid property name")),
    };
    if !word_in(values, value) {
        return Err(RegexErr("Invalid property value"));
    }
    Ok(())
}

const BINARY_PROPERTIES: &str = "ASCII ASCII_Hex_Digit AHex Alphabetic Alpha Any Assigned Bidi_Control Bidi_C Bidi_Mirrored Bidi_M Case_Ignorable CI Cased Changes_When_Casefolded CWCF Changes_When_Casemapped CWCM Changes_When_Lowercased CWL Changes_When_NFKC_Casefolded CWKCF Changes_When_Titlecased CWT Changes_When_Uppercased CWU Dash Default_Ignorable_Code_Point DI Deprecated Dep Diacritic Dia Emoji Emoji_Component Emoji_Modifier Emoji_Modifier_Base Emoji_Presentation Extender Ext Grapheme_Base Gr_Base Grapheme_Extend Gr_Ext Hex_Digit Hex IDS_Binary_Operator IDSB IDS_Trinary_Operator IDST ID_Continue IDC ID_Start IDS Ideographic Ideo Join_Control Join_C Logical_Order_Exception LOE Lowercase Lower Math Noncharacter_Code_Point NChar Pattern_Syntax Pat_Syn Pattern_White_Space Pat_WS Quotation_Mark QMark Radical Regional_Indicator RI Sentence_Terminal STerm Soft_Dotted SD Terminal_Punctuation Term Unified_Ideograph UIdeo Uppercase Upper Variation_Selector VS White_Space space XID_Continue XIDC XID_Start XIDS Extended_Pictographic EBase EComp EMod EPres ExtPict";

const BINARY_PROPERTIES_OF_STRINGS: &str = "Basic_Emoji Emoji_Keycap_Sequence RGI_Emoji_Modifier_Sequence RGI_Emoji_Flag_Sequence RGI_Emoji_Tag_Sequence RGI_Emoji_ZWJ_Sequence RGI_Emoji";

const GENERAL_CATEGORY_VALUES: &str = "Cased_Letter LC Close_Punctuation Pe Connector_Punctuation Pc Control Cc cntrl Currency_Symbol Sc Dash_Punctuation Pd Decimal_Number Nd digit Enclosing_Mark Me Final_Punctuation Pf Format Cf Initial_Punctuation Pi Letter L Letter_Number Nl Line_Separator Zl Lowercase_Letter Ll Mark M Combining_Mark Math_Symbol Sm Modifier_Letter Lm Modifier_Symbol Sk Nonspacing_Mark Mn Number N Open_Punctuation Ps Other C Other_Letter Lo Other_Number No Other_Punctuation Po Other_Symbol So Paragraph_Separator Zp Private_Use Co Punctuation P punct Separator Z Space_Separator Zs Spacing_Mark Mc Surrogate Cs Symbol S Titlecase_Letter Lt Unassigned Cn Uppercase_Letter Lu";

const SCRIPT_VALUES: &str = "Adlam Adlm Ahom Anatolian_Hieroglyphs Hluw Arabic Arab Armenian Armn Avestan Avst Balinese Bali Bamum Bamu Bassa_Vah Bass Batak Batk Bengali Beng Bhaiksuki Bhks Bopomofo Bopo Brahmi Brah Braille Brai Buginese Bugi Buhid Buhd Canadian_Aboriginal Cans Carian Cari Caucasian_Albanian Aghb Chakma Cakm Cham Cham Cherokee Cher Common Zyyy Coptic Copt Qaac Cuneiform Xsux Cypriot Cprt Cyrillic Cyrl Deseret Dsrt Devanagari Deva Duployan Dupl Egyptian_Hieroglyphs Egyp Elbasan Elba Ethiopic Ethi Georgian Geor Glagolitic Glag Gothic Goth Grantha Gran Greek Grek Gujarati Gujr Gurmukhi Guru Han Hani Hangul Hang Hanunoo Hano Hatran Hatr Hebrew Hebr Hiragana Hira Imperial_Aramaic Armi Inherited Zinh Qaai Inscriptional_Pahlavi Phli Inscriptional_Parthian Prti Javanese Java Kaithi Kthi Kannada Knda Katakana Kana Kayah_Li Kali Kharoshthi Khar Khmer Khmr Khojki Khoj Khudawadi Sind Lao Laoo Latin Latn Lepcha Lepc Limbu Limb Linear_A Lina Linear_B Linb Lisu Lisu Lycian Lyci Lydian Lydi Mahajani Mahj Malayalam Mlym Mandaic Mand Manichaean Mani Marchen Marc Masaram_Gondi Gonm Meetei_Mayek Mtei Mende_Kikakui Mend Meroitic_Cursive Merc Meroitic_Hieroglyphs Mero Miao Plrd Modi Mongolian Mong Mro Mroo Multani Mult Myanmar Mymr Nabataean Nbat New_Tai_Lue Talu Newa Newa Nko Nkoo Nushu Nshu Ogham Ogam Ol_Chiki Olck Old_Hungarian Hung Old_Italic Ital Old_North_Arabian Narb Old_Permic Perm Old_Persian Xpeo Old_South_Arabian Sarb Old_Turkic Orkh Oriya Orya Osage Osge Osmanya Osma Pahawh_Hmong Hmng Palmyrene Palm Pau_Cin_Hau Pauc Phags_Pa Phag Phoenician Phnx Psalter_Pahlavi Phlp Rejang Rjng Runic Runr Samaritan Samr Saurashtra Saur Sharada Shrd Shavian Shaw Siddham Sidd SignWriting Sgnw Sinhala Sinh Sora_Sompeng Sora Soyombo Soyo Sundanese Sund Syloti_Nagri Sylo Syriac Syrc Tagalog Tglg Tagbanwa Tagb Tai_Le Tale Tai_Tham Lana Tai_Viet Tavt Takri Takr Tamil Taml Tangut Tang Telugu Telu Thaana Thaa Thai Thai Tibetan Tibt Tifinagh Tfng Tirhuta Tirh Ugaritic Ugar Vai Vaii Warang_Citi Wara Yi Yiii Zanabazar_Square Zanb Dogra Dogr Gunjala_Gondi Gong Hanifi_Rohingya Rohg Makasar Maka Medefaidrin Medf Old_Sogdian Sogo Sogdian Sogd Elymaic Elym Nandinagari Nand Nyiakeng_Puachue_Hmong Hmnp Wancho Wcho Chorasmian Chrs Diak Dives_Akuru Khitan_Small_Script Kits Yezi Yezidi Cypro_Minoan Cpmn Old_Uyghur Ougr Tangsa Tnsa Toto Vithkuqi Vith Gara Garay Gukh Gurung_Khema Hrkt Katakana_Or_Hiragana Kawi Kirat_Rai Krai Nag_Mundari Nagm Ol_Onal Onao Sunu Sunuwar Todhri Todr Tulu_Tigalari Tutg Unknown Zzzz";
