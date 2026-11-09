//! Talking back to a CLI that is driven over stdin.
//!
//! The adapters read what a CLI says; this module writes what its host says
//! back: the next user turn, and the answer to a permission question. Kept
//! apart from the adapters because the adapter lives on the reader thread
//! and the replies are written from wherever the decision is made, and
//! because a CLI can be read without being driven.
//!
//! Like the adapters, this is pure: it formats lines and writes nothing.
//! [`crate::runtime::Running::write_line`] does the writing.

use serde_json::{json, Value};

/// A host's answer to [`crate::BrainEvent::PermissionRequested`].
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Run the tool with exactly the input that was asked about.
    Allow,
    /// Do not run it. The message reaches the model as the tool's error, so
    /// it can say why it stopped instead of guessing.
    ///
    /// `interrupt` also ends the turn. Without it a model that is refused
    /// usually tries another way -- live, Claude asked again for a different
    /// command straight after a denial -- and each attempt is one more
    /// question for a person to refuse.
    Deny { message: String, interrupt: bool },
}

/// How one CLI expects to be driven over stdin.
pub trait HostProtocol: Send + Sync {
    /// Which adapter this pairs with.
    fn id(&self) -> &'static str;

    /// Arguments that put the CLI into host-driven mode. Added by the caller
    /// only when the command does not already carry them.
    fn hosted_args(&self) -> &'static [&'static str];

    /// One user turn, as a stdin line.
    fn user_message(&self, text: &str) -> String;

    /// The reply to a permission question, as a stdin line. `input` is the
    /// input that was asked about; allowing echoes it back unchanged, so
    /// what runs is what was approved.
    fn permission_reply(&self, request_id: &str, decision: &Decision, input: &Value) -> String;
}

/// Claude Code over `--input-format stream-json --permission-prompt-tool stdio`.
///
/// Verified against Claude Code 2.1.319: a `control_request` with subtype
/// `can_use_tool` on stdout is answered with a `control_response` on stdin;
/// `allow` runs the tool, `deny` turns `message` into the tool's error.
pub struct ClaudeHost;

impl HostProtocol for ClaudeHost {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn hosted_args(&self) -> &'static [&'static str] {
        &[
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompt-tool",
            "stdio",
        ]
    }

    fn user_message(&self, text: &str) -> String {
        json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
    }

    fn permission_reply(&self, request_id: &str, decision: &Decision, input: &Value) -> String {
        let answer = match decision {
            Decision::Allow => json!({"behavior": "allow", "updatedInput": input}),
            Decision::Deny { message, interrupt } => {
                let mut answer = json!({"behavior": "deny", "message": message});
                if *interrupt {
                    answer["interrupt"] = json!(true);
                }
                answer
            }
        };
        json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": answer},
        })
        .to_string()
    }
}

/// The host protocol for an adapter, when the CLI behind it has one.
pub fn host_for(adapter: &str) -> Option<Box<dyn HostProtocol>> {
    match adapter {
        "claude" => Some(Box::new(ClaudeHost)),
        _ => None,
    }
}

/// `command` with the host's arguments added where it does not already say
/// them. A flag already present is left as the caller wrote it, value and
/// all: overriding a caller's explicit choice would be a surprise.
pub fn with_hosted_args(command: &[String], host: &dyn HostProtocol) -> Vec<String> {
    let mut out = command.to_vec();
    let wanted = host.hosted_args();
    let mut i = 0;
    while i < wanted.len() {
        let flag = wanted[i];
        let takes_value = wanted.get(i + 1).is_some_and(|next| !next.starts_with("--"));
        if !out.iter().any(|word| word == flag || word.starts_with(&format!("{flag}="))) {
            out.push(flag.to_string());
            if takes_value {
                out.push(wanted[i + 1].to_string());
            }
        }
        i += if takes_value { 2 } else { 1 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_replies_are_the_shapes_claude_reads() {
        let host = ClaudeHost;
        let allow: Value = serde_json::from_str(&host.permission_reply(
            "r1",
            &Decision::Allow,
            &json!({"command": "echo hi > out.txt"}),
        ))
        .unwrap();
        assert_eq!(allow["type"], "control_response");
        assert_eq!(allow["response"]["request_id"], "r1");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(allow["response"]["response"]["updatedInput"]["command"], "echo hi > out.txt");

        let deny: Value = serde_json::from_str(&host.permission_reply(
            "r2",
            &Decision::Deny { message: "not on this machine".into(), interrupt: false },
            &Value::Null,
        ))
        .unwrap();
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(deny["response"]["response"]["message"], "not on this machine");
        assert!(deny["response"]["response"].get("interrupt").is_none());

        let stop: Value = serde_json::from_str(&host.permission_reply(
            "r3",
            &Decision::Deny { message: "stop".into(), interrupt: true },
            &Value::Null,
        ))
        .unwrap();
        assert_eq!(stop["response"]["response"]["interrupt"], true);

        let user: Value = serde_json::from_str(&host.user_message("hello")).unwrap();
        assert_eq!(user, json!({"type": "user", "message": {"role": "user", "content": "hello"}}));
    }

    #[test]
    fn hosted_args_are_added_once_and_never_override_the_caller() {
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let out = with_hosted_args(&words(&["claude", "-p", "--model", "haiku"]), &ClaudeHost);
        assert_eq!(
            out,
            words(&[
                "claude", "-p", "--model", "haiku", "--input-format", "stream-json",
                "--output-format", "stream-json", "--verbose", "--permission-prompt-tool", "stdio",
            ])
        );
        // Already there (one as `--flag=value`): left exactly as written.
        let out = with_hosted_args(
            &words(&["claude", "-p", "--output-format", "stream-json", "--verbose", "--permission-prompt-tool=mcp__x__y"]),
            &ClaudeHost,
        );
        assert_eq!(out.iter().filter(|w| *w == "--output-format").count(), 1);
        assert_eq!(out.iter().filter(|w| *w == "--verbose").count(), 1);
        assert!(!out.contains(&"stdio".to_string()), "{out:?}");
        assert!(out.contains(&"--input-format".to_string()));
    }

    #[test]
    fn only_clis_with_a_host_protocol_get_one() {
        assert!(host_for("claude").is_some());
        for other in ["codex", "gemini", "opencode", "agy", "generic"] {
            assert!(host_for(other).is_none(), "{other}");
        }
    }
}
