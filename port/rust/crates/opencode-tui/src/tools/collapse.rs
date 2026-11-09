//! `util/collapse-tool-output.ts`: a tool's output (and a shell command with its output) cut to a number of lines
//! and characters, with an ellipsis, or the tail with an "(N earlier lines)" label.
use crate::util::{graphemes, string_width};

/// `{ output, overflow }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Collapsed {
    pub output: String,
    pub overflow: bool,
}

/// `{ input, output, overflow }`.
#[derive(Clone, Debug, PartialEq)]
pub struct CollapsedShell {
    pub input: String,
    pub output: String,
    pub overflow: bool,
}

/// `Array.from(text).length`: code points.
fn code_points(text: &str) -> usize {
    text.chars().count()
}

/// `collapseToolOutput(output, maxLines, maxChars)`.
pub fn collapse_tool_output(output: &str, max_lines: usize, max_chars: usize) -> Collapsed {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() <= max_lines && code_points(output) <= max_chars {
        return Collapsed { output: output.to_string(), overflow: false };
    }
    let mut visible: Vec<String> = lines.iter().take(max_lines).map(|l| l.to_string()).collect();
    if lines.len() > max_lines && !visible.is_empty() {
        visible.last_mut().unwrap().push('…');
    }
    let preview = visible.join("\n");
    if code_points(&preview) > max_chars {
        let kept: String = preview.chars().take(max_chars.saturating_sub(1)).collect();
        return Collapsed { output: kept + "…", overflow: true };
    }
    Collapsed { output: preview, overflow: true }
}

/// `collapseShellOutput(input, output, maxLines, maxChars)`.
pub fn collapse_shell_output(input: &str, output: &str, max_lines: usize, max_chars: usize) -> CollapsedShell {
    if input.is_empty() {
        let collapsed = collapse_tool_output(output, max_lines, max_chars);
        return CollapsedShell {
            input: input.to_string(),
            output: if collapsed.overflow { collapse_tail(output, max_lines, max_chars) } else { output.to_string() },
            overflow: collapsed.overflow,
        };
    }
    let command_lines = 2.min(max_lines);
    let line_chars = 1.max(max_chars / 1.max(max_lines));
    let command = collapse_shell_command(input, command_lines, line_chars);
    if output.is_empty() {
        return CollapsedShell { input: command.output, output: output.to_string(), overflow: command.overflow };
    }
    let lines = 1.max(max_lines as i64 - command.lines as i64 - 1) as usize;
    let chars = 1.max(max_chars as i64 - code_points(&command.output) as i64 - 2) as usize;
    let collapsed = collapse_tool_output(output, lines, chars);
    CollapsedShell {
        input: command.output,
        output: if collapsed.overflow { collapse_tail(output, lines, chars) } else { output.to_string() },
        overflow: command.overflow || collapsed.overflow,
    }
}

struct Command {
    output: String,
    overflow: bool,
    lines: usize,
}

/// `collapseShellCommand(input, maxLines, lineWidth)`.
fn collapse_shell_command(input: &str, max_lines: usize, line_width: usize) -> Command {
    let mut visible: Vec<&str> = Vec::new();
    let mut lines = 1;
    let mut width = 0usize;
    let mut overflow = false;
    for segment in graphemes(input) {
        if segment == "\n" {
            if lines >= max_lines {
                overflow = true;
                break;
            }
            visible.push(segment);
            lines += 1;
            width = 0;
            continue;
        }
        let next = string_width(segment) as usize;
        if width + next > line_width {
            if lines >= max_lines {
                overflow = true;
                break;
            }
            lines += 1;
            width = 0;
        }
        visible.push(segment);
        width += next;
    }
    if !overflow {
        return Command { output: input.to_string(), overflow, lines };
    }
    if width >= line_width {
        if let Some(removed) = visible.pop() {
            if removed != "\n" {
                width -= string_width(removed) as usize;
            }
        }
    }
    let _ = width;
    Command { output: visible.concat() + "…", overflow, lines }
}

/// `collapseTail(output, maxLines, maxChars)`.
fn collapse_tail(output: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() <= max_lines && code_points(output) <= max_chars {
        return output.to_string();
    }
    let count = 1.max(lines.len() as i64 - 0.max(max_lines as i64 - 1)) as usize;
    let label = format!("({count} earlier {})", if count == 1 { "line" } else { "lines" });
    if max_lines <= 1 {
        return label;
    }
    let tail = &lines[lines.len().saturating_sub(max_lines - 1)..];
    let preview: Vec<char> = tail.join("\n").chars().collect();
    let available = max_chars as i64 - code_points(&label) as i64 - 1;
    if available <= 0 {
        return label;
    }
    let start = preview.len().saturating_sub(available as usize);
    format!("{label}\n{}", preview[start..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell() {
        let output: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let collapsed = collapse_shell_output("$ sleep 1; seq 1 40", &output.join("\n"), 10, 10 * 88);
        assert!(collapsed.overflow);
        assert_eq!(collapsed.input, "$ sleep 1; seq 1 40");
        assert!(collapsed.output.starts_with("(33 earlier lines)\n34\n"), "{}", collapsed.output);
    }
}
