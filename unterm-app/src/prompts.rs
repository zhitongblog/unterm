//! Curated & user-extensible AI Prompt Template Library for Claude Code,
//! Codex CLI, and Antigravity (agy).
//!
//! Built-in templates cover the highest-frequency daily coding workflows
//! (diff review, regression tests, root-cause debugging, refactor, PR summary,
//! security/perf audit). Users can add their own templates in
//! `~/.unterm/prompts.json` as an array of `{ title, hint, prompt }` objects.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub title: String,
    #[serde(default)]
    pub hint: String,
    pub prompt: String,
}

/// Built-in templates tailored for AI Coding CLIs (localized for ZH/EN).
pub fn builtin_templates(is_zh: bool) -> Vec<PromptTemplate> {
    if is_zh {
        vec![
            PromptTemplate {
                title: "🔍 审查当前 Git Diff 并找出潜在 Bug 与回归".into(),
                hint: "Code Review · Claude / Codex / Agy".into(),
                prompt: "请运行 `git diff` 审查当前未提交改动，重点排查边界条件、并发竞争、错误处理和潜在回归风险，按严重程度列出问题并给出最小修复方案。".into(),
            },
            PromptTemplate {
                title: "🧪 为当前改动补齐高覆盖单元测试与边界用例".into(),
                hint: "Regression Tests".into(),
                prompt: "请分析当前 `git diff` 改动的核心逻辑，为正常路径和关键边界条件补齐单元测试，并运行测试套件验证全部通过。".into(),
            },
            PromptTemplate {
                title: "🐛 根因定位：结合最近报错日志做最小化修复".into(),
                hint: "Root-Cause Debug".into(),
                prompt: "请不要盲猜或绕过报错：先定位根本原因，用一句话说明触发链路，再给出最小、可验证的修复并跑通复现用例。".into(),
            },
            PromptTemplate {
                title: "⚡ 性能剖析与热点路径优化检查".into(),
                hint: "Performance Audit".into(),
                prompt: "请审查当前模块的热点路径，找出不必要的同步阻塞、重复分配、锁竞争或 O(n²) 逻辑，给出量化收益最高的优化方案。".into(),
            },
            PromptTemplate {
                title: "🛡️ 安全审计（注入 / 越权 / 密钥泄露 / 路径遍历）".into(),
                hint: "Security Audit".into(),
                prompt: "请对当前改动做一次安全审计，重点检查命令注入、路径遍历、未校验输入、权限绕过与敏感信息泄露，并列出修复建议。".into(),
            },
            PromptTemplate {
                title: "📝 生成简洁规范的 PR 描述与变更清单".into(),
                hint: "PR Summary".into(),
                prompt: "请基于 `git diff` 和最近提交，生成一份结构清晰的 PR 说明：背景动机、核心改动点、测试验证步骤与潜在风险。".into(),
            },
        ]
    } else {
        vec![
            PromptTemplate {
                title: "🔍 Review Current Git Diff for Bugs & Regressions".into(),
                hint: "Code Review · Claude / Codex / Agy".into(),
                prompt: "Run `git diff` and review all uncommitted changes for edge cases, race conditions, error handling gaps, and regressions. Rank findings by severity and propose minimal fixes.".into(),
            },
            PromptTemplate {
                title: "🧪 Write Unit & Edge-Case Regression Tests for Current Diff".into(),
                hint: "Regression Tests".into(),
                prompt: "Inspect the current `git diff`, add targeted unit tests covering happy paths and edge cases, and run the test suite to verify everything passes.".into(),
            },
            PromptTemplate {
                title: "🐛 Root-Cause Debug: Diagnose First, Minimal Fix Second".into(),
                hint: "Root-Cause Debug".into(),
                prompt: "Do not guess or work around the failure: trace the exact root cause first, explain the failure chain in one paragraph, then apply the minimal fix and verify with a test.".into(),
            },
            PromptTemplate {
                title: "⚡ Performance & Hot-Path Latency Audit".into(),
                hint: "Performance Audit".into(),
                prompt: "Audit the hot paths in this module for blocking I/O on UI threads, lock contention, redundant allocations, or O(n^2) loops, and propose the highest-impact optimizations.".into(),
            },
            PromptTemplate {
                title: "🛡️ Security Audit (Injection, Path Traversal, Secrets)".into(),
                hint: "Security Audit".into(),
                prompt: "Perform a security audit on the current changes focusing on command injection, path traversal, unvalidated input, auth bypasses, and secret leaks.".into(),
            },
            PromptTemplate {
                title: "📝 Generate Concise PR Description & Test Plan".into(),
                hint: "PR Summary".into(),
                prompt: "Based on `git diff` and recent commits, draft a crisp PR description covering motivation, key changes, verification steps, and any rollback notes.".into(),
            },
        ]
    }
}

/// Load user-defined prompt templates from `path` (e.g. `~/.unterm/prompts.json`)
/// and prepend them ahead of the built-in templates.
pub fn load_all_templates(user_file: Option<&Path>, is_zh: bool) -> Vec<PromptTemplate> {
    let mut out = Vec::new();
    if let Some(path) = user_file {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(custom) = serde_json::from_str::<Vec<PromptTemplate>>(&raw) {
                for item in custom {
                    if !item.title.trim().is_empty() && !item.prompt.trim().is_empty() {
                        out.push(item);
                    }
                }
            }
        }
    }
    out.extend(builtin_templates(is_zh));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_custom_prompts_ahead_of_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompts.json");
        std::fs::write(
            &path,
            r#"[{"title":"Custom Release Check","hint":"Team","prompt":"Verify changelog and Cargo.toml"}]"#,
        )
        .unwrap();

        let all = load_all_templates(Some(&path), false);
        assert!(all.len() >= 7);
        assert_eq!(all[0].title, "Custom Release Check");
        assert_eq!(all[0].prompt, "Verify changelog and Cargo.toml");
    }

    #[test]
    fn falls_back_cleanly_on_missing_or_invalid_user_file() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        let zh = load_all_templates(Some(&bad), true);
        assert_eq!(zh.len(), 6);
        assert!(zh[0].title.contains("审查"));
    }
}
