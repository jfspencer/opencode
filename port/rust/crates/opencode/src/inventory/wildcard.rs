//! `Wildcard.match` (packages/core/src/util/wildcard.ts): `*` matches any run of characters (newlines included), `?`
//! one character, `[*]` and `[?]` are a literal `*` and `?`, everything else is literal; backslashes compare as `/`.
//! A pattern ending in ` *` also matches without the space and what follows (`ls *` matches `ls`).

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Any,
    One,
    Char(char),
}

fn tokens(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' if i + 2 < chars.len() && matches!(chars[i + 1], '*' | '?') && chars[i + 2] == ']' => {
                out.push(Token::Char(chars[i + 1]));
                i += 3;
                continue;
            }
            '*' => out.push(Token::Any),
            '?' => out.push(Token::One),
            c => out.push(Token::Char(c)),
        }
        i += 1;
    }
    out
}

fn matches(input: &[char], pattern: &[Token]) -> bool {
    // Iterative glob matching with backtracking to the last `*`.
    let (mut i, mut p) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < input.len() {
        match pattern.get(p) {
            Some(Token::Any) => {
                star = Some((p, i));
                p += 1;
            }
            Some(Token::One) => {
                i += 1;
                p += 1;
            }
            Some(Token::Char(c)) if *c == input[i] => {
                i += 1;
                p += 1;
            }
            _ => match star {
                Some((sp, si)) => {
                    p = sp + 1;
                    i = si + 1;
                    star = Some((sp, si + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|t| *t == Token::Any)
}

pub fn wildcard_match(input: &str, pattern: &str) -> bool {
    let input: Vec<char> = input.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
    let tokens = tokens(pattern);
    if tokens.len() >= 2 && tokens[tokens.len() - 2..] == [Token::Char(' '), Token::Any] {
        // `( .*)?`: the base alone, or the base followed by a space and anything.
        let base = &tokens[..tokens.len() - 2];
        return matches(&input, base) || matches(&input, &tokens);
    }
    matches(&input, &tokens)
}

#[cfg(test)]
mod tests {
    use super::wildcard_match as m;

    #[test]
    fn globs() {
        assert!(m("openrouter", "*"));
        assert!(m("openrouter", "openrouter"));
        assert!(!m("opencode", "openrouter"));
        assert!(m("group-inner", "group*"));
        assert!(m("a\nb", "a*"));
        assert!(m("ls", "ls *"));
        assert!(m("ls -la", "ls *"));
        assert!(!m("lsx", "ls *"));
        assert!(m("a*b", "a[*]b"));
        assert!(!m("axb", "a[*]b"));
        assert!(m("abc", "a?c"));
        assert!(m("C:/x", "C:\\x"));
    }
}
