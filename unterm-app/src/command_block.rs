//! Command Block Actions — extract the last executed command and its clean output
//! from terminal screen/scrollback lines for 1-click copy or AI agent diagnosis.
//!
//! Traditional terminals force developers to scroll up hundreds of lines and
//! drag-select stdout/stderr with the mouse. This module identifies prompt
//! boundaries in screen/scrollback text so the user (or an MCP tool) can grab
//! the most recent command's clean output in one action.

/// Extracted command block from the bottom of a terminal pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandBlock {
    /// The command line that was executed, if a prompt line was recognized.
    pub command: Option<String>,
    /// Clean stdout/stderr lines belonging to that command.
    pub output_lines: Vec<String>,
    /// Exit code reported via `OSC 133;D`, if known.
    pub exit_code: Option<i32>,
    /// Elapsed wall-clock duration in milliseconds, if known.
    pub elapsed_ms: Option<u64>,
}

impl CommandBlock {
    /// Clean multi-line output text suitable for copying to the clipboard.
    pub fn clean_output(&self) -> String {
        self.output_lines.join("\n")
    }

    /// Format a compact duration badge (`420ms`, `4.2s`, `1m 12s`).
    pub fn duration_badge(elapsed_ms: u64) -> String {
        if elapsed_ms < 1000 {
            format!("{elapsed_ms}ms")
        } else if elapsed_ms < 60_000 {
            format!("{:.1}s", elapsed_ms as f64 / 1000.0)
        } else {
            let secs = elapsed_ms / 1000;
            format!("{}m {}s", secs / 60, secs % 60)
        }
    }

    /// Build a one-line prompt to feed directly into an AI coding agent pane.
    pub fn agent_fix_prompt(&self, is_zh: bool) -> String {
        let cmd_part = self
            .command
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|c| format!("`{c}`"))
            .unwrap_or_else(|| {
                if is_zh {
                    "上一条终端命令".to_string()
                } else {
                    "the last command".to_string()
                }
            });
        let exit_part = self
            .exit_code
            .filter(|&c| c != 0)
            .map(|c| format!(" (exit {c})"))
            .unwrap_or_default();
        let tail: Vec<&str> = self
            .output_lines
            .iter()
            .rev()
            .take(25)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let excerpt = tail.join(" ⏎ ");
        if is_zh {
            format!(
                "请分析并修复 {cmd_part}{exit_part} 的报错输出：{excerpt}\r"
            )
        } else {
            format!(
                "Please diagnose and fix the failure from {cmd_part}{exit_part}: {excerpt}\r"
            )
        }
    }
}

/// Heuristic check for whether a terminal line looks like a shell prompt line.
fn parse_prompt_line(line: &str) -> Option<Option<String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // PowerShell prompt: `PS C:\Users\...> command` or `PS /Users/...> command`
    if let Some(rest) = trimmed.strip_prefix("PS ") {
        if let Some((_, after_gt)) = rest.split_once(">") {
            let cmd = after_gt.trim();
            return Some((!cmd.is_empty()).then(|| cmd.to_string()));
        }
    }

    // Standard POSIX / Starship / Oh-My-Zsh prompts ending or splitting on `$ `, `% `, `❯ `, `➜ `, `# `
    for marker in [" ❯ ", " ➜ ", "$ ", "% ", "# "] {
        if let Some((prefix, after)) = trimmed.split_once(marker) {
            // Avoid matching arbitrary output lines that happen to contain `$ ` mid-sentence.
            if prefix.len() <= 80 && !prefix.contains("http://") && !prefix.contains("https://") {
                let cmd = after.trim();
                return Some((!cmd.is_empty()).then(|| cmd.to_string()));
            }
        }
    }

    // Bare trailing prompt waiting for input at the bottom of the screen (`❯`, `$`, `%`, `>`).
    if matches!(trimmed, "❯" | "➜" | "$" | "%" | ">")
        || ((trimmed.ends_with(" $")
            || trimmed.ends_with(" %")
            || trimmed.ends_with(" ❯")
            || trimmed.ends_with(" #"))
            && trimmed.len() <= 80)
    {
        return Some(None);
    }

    None
}

/// Extract the most recent command and its output from terminal screen lines.
pub fn extract_last_command(
    lines: &[String],
    exit_code: Option<i32>,
    elapsed_ms: Option<u64>,
) -> CommandBlock {
    // 1. Trim trailing blank lines.
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    if end == 0 {
        return CommandBlock {
            command: None,
            output_lines: Vec::new(),
            exit_code,
            elapsed_ms,
        };
    }

    // 2. If the very last non-empty line is an idle prompt waiting for input, step above it.
    if matches!(parse_prompt_line(&lines[end - 1]), Some(None)) && end > 1 {
        end -= 1;
        while end > 0 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
    }

    // 3. Walk upwards to find the prompt line that launched the last command (up to 160 lines).
    let search_floor = end.saturating_sub(160);
    let mut cmd_prompt_idx = None;
    let mut parsed_cmd = None;
    for idx in (search_floor..end).rev() {
        if let Some(Some(cmd)) = parse_prompt_line(&lines[idx]) {
            cmd_prompt_idx = Some(idx);
            parsed_cmd = Some(cmd);
            break;
        }
    }

    let start = match cmd_prompt_idx {
        Some(idx) => idx + 1,
        None => end.saturating_sub(40),
    };

    let output_lines: Vec<String> = lines[start..end]
        .iter()
        .map(|l| l.trim_end().to_string())
        .collect();

    CommandBlock {
        command: parsed_cmd,
        output_lines,
        exit_code,
        elapsed_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_command_and_clean_output_between_prompts() {
        let screen = vec![
            "alex@mac ~/code/unterm % cargo test -p unterm-app".to_string(),
            "   Compiling unterm-app v0.71.30".to_string(),
            "error[E0308]: mismatched types".to_string(),
            "  --> src/window.rs:42:5".to_string(),
            "alex@mac ~/code/unterm % ".to_string(),
            "".to_string(),
        ];
        let block = extract_last_command(&screen, Some(101), Some(4250));
        assert_eq!(block.command.as_deref(), Some("cargo test -p unterm-app"));
        assert_eq!(
            block.output_lines,
            vec![
                "   Compiling unterm-app v0.71.30".to_string(),
                "error[E0308]: mismatched types".to_string(),
                "  --> src/window.rs:42:5".to_string(),
            ]
        );
        assert_eq!(block.exit_code, Some(101));
        assert_eq!(CommandBlock::duration_badge(4250), "4.2s");
    }

    #[test]
    fn extracts_powershell_prompt_command_block() {
        let screen = vec![
            r"PS C:\Users\alex\code\unterm> npm run build".to_string(),
            "✓ built in 2.81s".to_string(),
            r"PS C:\Users\alex\code\unterm>".to_string(),
        ];
        let block = extract_last_command(&screen, Some(0), Some(2810));
        assert_eq!(block.command.as_deref(), Some("npm run build"));
        assert_eq!(block.clean_output(), "✓ built in 2.81s");
    }
}
