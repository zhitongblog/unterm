//! Talking back to a CLI that is driven over stdin.
//!
//! The adapters read what a CLI says; this module writes what its host says
//! back: the next user turn, and the answer to a permission question. Kept
//! apart from the adapters because the adapter lives on the reader thread
//! and the replies are written from wherever the decision is made, and
//! because a CLI can be read without being driven.
//!
//! It formats lines and writes nothing: [`crate::runtime::Running::write_line`]
//! and the runtime's tap do the writing. A request/response protocol (ACP,
//! see [`crate::acp`]) has to remember what the CLI said to answer it, so a
//! host may keep state; a stream protocol like Claude's needs none.

use serde_json::{json, Value};

use crate::BrainAdapter;

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
///
/// Every method that produces output returns the stdin lines to write, in
/// order -- possibly none (a prompt held until the session exists), possibly
/// several (a refusal followed by a cancel).
pub trait HostProtocol: Send + Sync {
    /// Which adapter this pairs with.
    fn id(&self) -> &'static str;

    /// Arguments that put the CLI into host-driven mode. Added by the caller
    /// only when the command does not already carry them.
    fn hosted_args(&self) -> &'static [&'static str];

    /// The adapter that reads the CLI in this mode, when it is not the one
    /// that reads its plain output (`opencode acp` is not `opencode run`).
    fn adapter(&self) -> Option<Box<dyn BrainAdapter>> {
        None
    }

    /// The command in host-driven mode. By default the [`hosted_args`]
    /// added where missing; an error when the command cannot be put into
    /// that mode without guessing.
    ///
    /// [`hosted_args`]: HostProtocol::hosted_args
    fn hosted_command(&self, command: &[String]) -> anyhow::Result<Vec<String>> {
        Ok(with_args(command, self.hosted_args()))
    }

    /// Continue the conversation `external_id` (the CLI's own id for it)
    /// instead of starting a new one: the command to run, and whatever the
    /// host must remember to do so. An error when this protocol cannot
    /// resume here -- never a fresh conversation that looks like the old one.
    fn resume(&self, command: Vec<String>, external_id: &str) -> anyhow::Result<Vec<String>> {
        let _ = (command, external_id);
        Err(anyhow::anyhow!("a hosted {} session cannot resume a conversation yet", self.id()))
    }

    /// Environment the mode needs. The caller's own values win.
    fn hosted_env(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Lines written before anything else (a handshake).
    fn opening(&self, _cwd: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    /// Sees every stdout line, before the adapter; returns lines to write
    /// in response.
    fn observe(&self, _line: &str) -> Vec<String> {
        Vec::new()
    }

    /// One user turn.
    fn user_message(&self, text: &str) -> Vec<String>;

    /// The reply to a permission question. `input` is the input that was
    /// asked about; allowing echoes it back unchanged where the protocol
    /// carries it, so what runs is what was approved.
    fn permission_reply(&self, request_id: &str, decision: &Decision, input: &Value) -> Vec<String>;
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

    fn user_message(&self, text: &str) -> Vec<String> {
        vec![json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()]
    }

    /// `--resume <session_id>`: Claude Code reloads the conversation and
    /// takes the next stream-json message as its next turn.
    fn resume(&self, mut command: Vec<String>, external_id: &str) -> anyhow::Result<Vec<String>> {
        if external_id.trim().is_empty() {
            anyhow::bail!("no conversation id to resume");
        }
        if !command.iter().any(|word| word == "--resume" || word.starts_with("--resume=")) {
            command.push("--resume".into());
            command.push(external_id.to_string());
        }
        Ok(command)
    }

    fn permission_reply(&self, request_id: &str, decision: &Decision, input: &Value) -> Vec<String> {
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
        vec![json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": answer},
        })
        .to_string()]
    }
}

/// The host protocol for an adapter, when the CLI behind it has one.
pub fn host_for(adapter: &str) -> Option<Box<dyn HostProtocol>> {
    match adapter {
        "claude" => Some(Box::new(ClaudeHost)),
        "opencode" => Some(Box::new(crate::acp::AcpHost::opencode())),
        "codex" => Some(Box::new(crate::codex_app::CodexAppHost::new())),
        "agy" => Some(Box::new(crate::acp::AcpHost::agy())),
        _ => None,
    }
}

/// `command` with the host's arguments added where it does not already say
/// them. A flag already present is left as the caller wrote it, value and
/// all: overriding a caller's explicit choice would be a surprise.
pub fn with_hosted_args(command: &[String], host: &dyn HostProtocol) -> Vec<String> {
    with_args(command, host.hosted_args())
}

/// A program's bare name: `C:\bin\opencode.cmd` is `opencode`.
pub fn program_name(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base
}

/// `command` with `sub` right after the program `program` (`opencode` ->
/// `opencode acp`, `cmd /c codex` -> `cmd /c codex app-server`).
///
/// Left alone when it already names `sub`, or when it never names the
/// program at all -- then it is the caller's own launcher (a wrapper script),
/// taken as already in that mode. Another subcommand there (`opencode run`,
/// `codex exec`) is an error rather than a rewrite: its flags belong to it,
/// and guessing which of them `sub` accepts would launch something other
/// than what was asked for.
pub fn with_subcommand(command: &[String], program: &str, sub: &str) -> anyhow::Result<Vec<String>> {
    let mut out = command.to_vec();
    if out.iter().any(|word| word == sub) {
        return Ok(out);
    }
    let Some(at) = out.iter().position(|word| program_name(word) == program) else {
        return Ok(out);
    };
    if let Some(next) = out.get(at + 1).filter(|word| !word.starts_with('-')) {
        anyhow::bail!(
            "a hosted {program} session runs `{program} {sub}`, not `{program} {next}`: pass the command without `{next}`"
        );
    }
    out.insert(at + 1, sub.to_string());
    Ok(out)
}

fn with_args(command: &[String], wanted: &[&str]) -> Vec<String> {
    let mut out = command.to_vec();
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
        let one = |lines: Vec<String>| -> String {
            assert_eq!(lines.len(), 1, "{lines:?}");
            lines.into_iter().next().unwrap()
        };
        let allow: Value = serde_json::from_str(&one(host.permission_reply(
            "r1",
            &Decision::Allow,
            &json!({"command": "echo hi > out.txt"}),
        )))
        .unwrap();
        assert_eq!(allow["type"], "control_response");
        assert_eq!(allow["response"]["request_id"], "r1");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(allow["response"]["response"]["updatedInput"]["command"], "echo hi > out.txt");

        let deny: Value = serde_json::from_str(&one(host.permission_reply(
            "r2",
            &Decision::Deny { message: "not on this machine".into(), interrupt: false },
            &Value::Null,
        )))
        .unwrap();
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(deny["response"]["response"]["message"], "not on this machine");
        assert!(deny["response"]["response"].get("interrupt").is_none());

        let stop: Value = serde_json::from_str(&one(host.permission_reply(
            "r3",
            &Decision::Deny { message: "stop".into(), interrupt: true },
            &Value::Null,
        )))
        .unwrap();
        assert_eq!(stop["response"]["response"]["interrupt"], true);

        let user: Value = serde_json::from_str(&one(host.user_message("hello"))).unwrap();
        assert_eq!(user, json!({"type": "user", "message": {"role": "user", "content": "hello"}}));
    }

    #[test]
    fn claude_resumes_with_its_session_id_and_acp_refuses() {
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ClaudeHost.resume(words(&["claude", "-p"]), "s1").unwrap(),
            words(&["claude", "-p", "--resume", "s1"])
        );
        assert!(ClaudeHost.resume(words(&["claude"]), "").is_err());
        // ACP's session/load replays the whole history as updates: not
        // offered until that can be told apart from a new turn.
        assert!(host_for("opencode").unwrap().resume(words(&["opencode", "acp"]), "ses_1").is_err());
        assert!(host_for("agy").unwrap().resume(words(&["agy", "acp"]), "c2aa").is_err());
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
        assert!(host_for("opencode").is_some());
        assert!(host_for("codex").is_some());
        assert!(host_for("agy").is_some());
        for other in ["gemini", "generic"] {
            assert!(host_for(other).is_none(), "{other}");
        }
    }
}
