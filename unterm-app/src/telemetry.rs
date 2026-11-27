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

/// Parse a cost label like `"$1.28"` into USD (`1.28`).
pub fn parse_cost_usd(label: &str) -> Option<f64> {
    let trimmed = label.trim().trim_start_matches('$');
    let val = trimmed.parse::<f64>().ok()?;
    (val.is_finite() && val >= 0.0).then_some(val)
}

/// Parse a token label like `"142.5k"` or `"1.2m"` into an integer token count.
pub fn parse_tokens_count(label: &str) -> Option<u64> {
    let lower = label.trim().to_ascii_lowercase();
    if let Some(num) = lower.strip_suffix('k') {
        let v = num.parse::<f64>().ok()?;
        return (v.is_finite() && v >= 0.0).then_some((v * 1_000.0).round() as u64);
    }
    if let Some(num) = lower.strip_suffix('m') {
        let v = num.parse::<f64>().ok()?;
        return (v.is_finite() && v >= 0.0).then_some((v * 1_000_000.0).round() as u64);
    }
    lower.replace(',', "").parse::<u64>().ok()
}

/// Format an aggregate token count concisely (`142.5k`, `1.2M`, `850`).
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.2}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Cross-session daily aggregate telemetry for a single AI agent CLI.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AgentDailyStats {
    pub agent: String,
    pub sessions_seen: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub max_context_percent: u8,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
}

impl AgentDailyStats {
    pub fn success_rate_pct(&self) -> Option<u8> {
        let total = self.completed_tasks + self.failed_tasks;
        if total == 0 {
            return None;
        }
        Some(((self.completed_tasks * 100) / total).min(100) as u8)
    }

    pub fn summary_line(&self) -> String {
        let mut parts = vec![format!("{} tok", format_tokens(self.total_tokens))];
        if self.total_cost_usd > 0.0 {
            parts.push(format!("${:.2}", self.total_cost_usd));
        }
        if self.max_context_percent > 0 {
            parts.push(format!("peak ctx {}%", self.max_context_percent));
        }
        if let Some(rate) = self.success_rate_pct() {
            parts.push(format!(
                "success {rate}% ({}/{})",
                self.completed_tasks,
                self.completed_tasks + self.failed_tasks
            ));
        }
        parts.join(" · ")
    }
}

/// Cross-session daily telemetry summary across all agents.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DailyTelemetrySummary {
    pub day_index: u64,
    pub agents: Vec<AgentDailyStats>,
}

impl DailyTelemetrySummary {
    pub fn today_index() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0)
    }

    pub fn record_sample(
        &mut self,
        agent: &str,
        sample: &PaneTelemetry,
        outcome: Option<bool>,
    ) {
        let key = agent.trim().to_ascii_lowercase();
        if key.is_empty() {
            return;
        }
        let entry = match self.agents.iter_mut().find(|a| a.agent == key) {
            Some(existing) => existing,
            None => {
                self.agents.push(AgentDailyStats {
                    agent: key.clone(),
                    ..Default::default()
                });
                self.agents.last_mut().unwrap()
            }
        };
        entry.sessions_seen += 1;
        if let Some(tokens) = sample.tokens_label.as_deref().and_then(parse_tokens_count) {
            entry.total_tokens = entry.total_tokens.saturating_add(tokens);
        }
        if let Some(cost) = sample.cost_label.as_deref().and_then(parse_cost_usd) {
            entry.total_cost_usd += cost;
        }
        if let Some(pct) = sample.context_percent {
            entry.max_context_percent = entry.max_context_percent.max(pct);
        }
        match outcome {
            Some(true) => entry.completed_tasks += 1,
            Some(false) => entry.failed_tasks += 1,
            None => {}
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.agents.iter().map(|a| a.total_tokens).sum()
    }

    pub fn total_cost_usd(&self) -> f64 {
        self.agents.iter().map(|a| a.total_cost_usd).sum()
    }

    /// Evaluate today's spend against a user-configured daily USD budget
    /// (`UNTERM_DAILY_BUDGET_USD`, or `None` when disabled/zero).
    pub fn budget_alert(&self, daily_budget_usd: Option<f64>) -> Option<BudgetAlert> {
        let limit = daily_budget_usd.filter(|v| v.is_finite() && *v > 0.0)?;
        let spent = self.total_cost_usd();
        let pct = ((spent / limit) * 100.0).round().clamp(0.0, 999.0) as u16;
        let level = if pct >= 100 {
            BudgetLevel::Exceeded
        } else if pct >= 80 {
            BudgetLevel::Warning
        } else {
            BudgetLevel::Normal
        };
        Some(BudgetAlert {
            spent_usd: spent,
            limit_usd: limit,
            percent: pct,
            level,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BudgetLevel {
    Normal,
    /// >= 80% of daily budget consumed.
    Warning,
    /// >= 100% of daily budget consumed.
    Exceeded,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BudgetAlert {
    pub spent_usd: f64,
    pub limit_usd: f64,
    pub percent: u16,
    pub level: BudgetLevel,
}

impl BudgetAlert {
    pub fn badge(&self) -> String {
        let icon = match self.level {
            BudgetLevel::Exceeded => "⛔ ",
            BudgetLevel::Warning => "⚠ ",
            BudgetLevel::Normal => "",
        };
        format!(
            "{icon}${:.2} / ${:.2} ({}%)",
            self.spent_usd, self.limit_usd, self.percent
        )
    }
}

/// Reads optional daily USD spend budget from `UNTERM_DAILY_BUDGET_USD`.
pub fn configured_daily_budget_usd() -> Option<f64> {
    std::env::var("UNTERM_DAILY_BUDGET_USD")
        .ok()
        .and_then(|raw| raw.trim().trim_start_matches('$').parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
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

    #[test]
    fn aggregates_daily_tokens_cost_and_success_rate_across_sessions() {
        let mut summary = DailyTelemetrySummary {
            day_index: 20000,
            agents: Vec::new(),
        };
        let s1 = extract_from_lines(&["142.5k tokens · $1.20 · ctx: 72%".to_string()]);
        let s2 = extract_from_lines(&["57.5k tokens · $0.80 · ctx: 88%".to_string()]);
        summary.record_sample("claude", &s1, Some(true));
        summary.record_sample("claude", &s2, Some(false));
        assert_eq!(summary.total_tokens(), 200_000);
        assert!((summary.total_cost_usd() - 2.0).abs() < 1e-6);
        assert_eq!(summary.agents[0].max_context_percent, 88);
        assert_eq!(summary.agents[0].success_rate_pct(), Some(50));
        assert!(summary.agents[0].summary_line().contains("200.0k tok"));
        assert!(summary.agents[0].summary_line().contains("$2.00"));
    }

    #[test]
    fn evaluates_daily_budget_thresholds_and_badges() {
        let mut summary = DailyTelemetrySummary {
            day_index: 20001,
            agents: Vec::new(),
        };
        let s1 = extract_from_lines(&["100k tokens · $8.50 · ctx: 60%".to_string()]);
        summary.record_sample("claude", &s1, Some(true));
        let warn = summary.budget_alert(Some(10.0)).expect("budget alert");
        assert_eq!(warn.level, BudgetLevel::Warning);
        assert_eq!(warn.percent, 85);
        assert!(warn.badge().contains("⚠ $8.50 / $10.00 (85%)"));

        let s2 = extract_from_lines(&["50k tokens · $2.00 · ctx: 70%".to_string()]);
        summary.record_sample("codex", &s2, Some(true));
        let over = summary.budget_alert(Some(10.0)).expect("budget alert");
        assert_eq!(over.level, BudgetLevel::Exceeded);
        assert_eq!(over.percent, 105);
        assert!(over.badge().contains("⛔ $10.50 / $10.00 (105%)"));
        assert!(summary.budget_alert(Some(0.0)).is_none());
    }
}
