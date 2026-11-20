//! AI Agent Telemetry, Context Pressure & Token/Cost HUD.
//!
//! Parses live terminal lines from active CLI agents (Claude Code, Codex,
//! Antigravity/Gemini, OpenCode, Aider) to extract context-window fullness
//! (`ctx 78%`), token counts (`142k tokens`), and session cost (`$0.84`)
//! without invading the agent's process or requiring API keys.
//!
//! When an agent's context window approaches 75%+, surfaces a non-intrusive
//! warning and offers a 1-click `/compact` or Cross-Agent Handoff action.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPressure {
    Normal,
    /// 70%..85% full — worth compacting at the next natural pause.
    Elevated,
    /// >= 85% full — risk of auto-compaction or truncation mid-task.
    Critical,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneTelemetry {
    /// Context window occupancy percentage (`0..=100`), if reported on screen.
    pub context_percent: Option<u8>,
    /// Token count string as displayed by the agent (`148k`, `42.5k`, etc.).
    pub tokens_label: Option<String>,
    /// Reported session cost (`$1.24`), if present.
    pub cost_label: Option<String>,
}

impl PaneTelemetry {
    pub fn pressure(&self) -> ContextPressure {
        match self.context_percent.unwrap_or(0) {
            85..=u8::MAX => ContextPressure::Critical,
            70..=84 => ContextPressure::Elevated,
            _ => ContextPressure::Normal,
        }
    }

    /// Compact summary badge for the Cockpit Inbox or Sidebar (`ctx 82% · $0.45`).
    pub fn summary_badge(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(pct) = self.context_percent {
            let warn = if pct >= 85 {
                "⚠ "
            } else if pct >= 70 {
                "● "
            } else {
                ""
            };
            parts.push(format!("{warn}ctx {pct}%"));
        } else if let Some(tokens) = &self.tokens_label {
            parts.push(format!("{tokens} tok"));
        }
        if let Some(cost) = &self.cost_label {
            parts.push(cost.clone());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Scan recent terminal lines (bottom-up) for context window %, token usage, and cost.
pub fn extract_from_lines(lines: &[String]) -> PaneTelemetry {
    let mut out = PaneTelemetry::default();
    for line in lines.iter().rev().take(30) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();

        if out.context_percent.is_none() {
            out.context_percent = parse_context_percent(&lower);
        }
        if out.cost_label.is_none() {
            out.cost_label = parse_cost_label(trimmed);
        }
        if out.tokens_label.is_none() {
            out.tokens_label = parse_tokens_label(&lower);
        }
        if out.context_percent.is_some() && out.cost_label.is_some() && out.tokens_label.is_some() {
            break;
        }
    }
    out
}

fn parse_context_percent(lower: &str) -> Option<u8> {
    // Match common CLI status bar patterns:
    // - "context: 78%" / "context 78%" / "ctx: 82%" / "ctx 82%"
    // - "78% context" / "22% context left" (convert remaining -> used!)
    for marker in ["context left", "context remaining", "ctx left"] {
        if let Some(idx) = lower.find(marker) {
            if let Some(pct) = extract_Trailing_percent(&lower[..idx]) {
                return Some(100u8.saturating_sub(pct.min(100)));
            }
        }
    }
    for marker in ["context:", "context ", "ctx:", "ctx "] {
        if let Some(idx) = lower.find(marker) {
            let after = &lower[idx + marker.len()..];
            if let Some(pct) = extract_leading_percent(after) {
                return Some(pct.min(100));
            }
        }
    }
    for marker in ["% context", "% ctx"] {
        if let Some(idx) = lower.find(marker) {
            if let Some(pct) = extract_Trailing_percent(&lower[..idx]) {
                return Some(pct.min(100));
            }
        }
    }
    None
}

fn extract_leading_percent(s: &str) -> Option<u8> {
    let s = s.trim_start();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = s[digits.len()..].trim_start();
    if !after.starts_with('%') {
        return None;
    }
    digits.parse::<u8>().ok()
}

#[allow(non_snake_case)]
fn extract_Trailing_percent(before: &str) -> Option<u8> {
    let trimmed = before.trim_end().trim_end_matches('%').trim_end();
    let digits: String = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse::<u8>().ok()
}

fn parse_cost_label(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'$' {
            let rest = &line[i + 1..];
            let num: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if num.contains('.') && num.len() >= 3 && num.parse::<f64>().is_ok() {
                return Some(format!("${num}"));
            }
        }
    }
    None
}

fn parse_tokens_label(lower: &str) -> Option<String> {
    for marker in [" tokens", " tok", "tk "] {
        if let Some(idx) = lower.find(marker) {
            let before = lower[..idx].trim_end();
            let token_word: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | 'k' | 'm'))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if token_word.chars().any(|c| c.is_ascii_digit()) && token_word.len() >= 2 {
                return Some(token_word);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_context_percent_tokens_and_cost() {
        let lines = vec![
            "claude-3-7-sonnet · 142.5k tokens · $1.28 · ctx: 84%".to_string(),
        ];
        let t = extract_from_lines(&lines);
        assert_eq!(t.context_percent, Some(84));
        assert_eq!(t.tokens_label.as_deref(), Some("142.5k"));
        assert_eq!(t.cost_label.as_deref(), Some("$1.28"));
        assert_eq!(t.pressure(), ContextPressure::Elevated);
        assert_eq!(t.summary_badge().as_deref(), Some("● ctx 84% · $1.28"));
    }

    #[test]
    fn converts_context_left_to_occupancy() {
        let lines = vec!["12% context left · auto-compact soon".to_string()];
        let t = extract_from_lines(&lines);
        assert_eq!(t.context_percent, Some(88));
        assert_eq!(t.pressure(), ContextPressure::Critical);
        assert_eq!(t.summary_badge().as_deref(), Some("⚠ ctx 88%"));
    }
}
