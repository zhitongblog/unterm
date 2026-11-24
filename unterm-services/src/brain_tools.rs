//! Where a model's tool request meets the gateway.
//!
//! A brain adapter produces a *request*: the model would like a shell run, a
//! file written, a pane killed. Nothing in `unterm-brain` can perform one —
//! there is no gateway there, and no code that would call it. This is the
//! only bridge, and it goes through the same door as everything else.
//!
//! The translation is the interesting part. A model says `Bash` or `shell` or
//! `run_command`; the gateway judges `exec.run`. Naming a tool the gateway
//! does not know must not become permission, so an unrecognised tool is
//! judged as a mutation and asked about, never waved through. That asymmetry
//! is deliberate: the cost of asking about something harmless is a prompt,
//! and the cost of the other mistake is whatever the tool did.

use crate::gateway::{admit, Outcome, Passage, SettingsPolicy};
use serde_json::Value;
use unterm_brain::BrainEvent;
use unterm_gateway::{ActionContext, Entry, PolicySource};

/// Who is asking, and what for.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    /// The agent's name, for the audit trail and for actor-scoped grants.
    pub actor: Option<String>,
    /// Where the session's shell is *now*, if the caller knows.
    ///
    /// Judged on every call rather than remembered: a shell can `cd`
    /// anywhere, and the answer from when the session started is about a
    /// directory it may have left twenty commands ago.
    pub cwd: Option<String>,
    /// The task this turn belongs to, so one approval can cover it.
    pub task_id: Option<String>,
    /// The workspace this turn is confined to, when it has one.
    ///
    /// `None` means unconfined, which is the old behaviour and still the
    /// common one: most agents are driving a terminal the user is sitting at,
    /// not working inside a bounded root. When it *is* set, a path outside
    /// that root is refused before the gateway is consulted — asking the user
    /// to approve reading a file the workspace was defined to exclude would
    /// be asking them to undo their own boundary one prompt at a time.
    pub workspace_id: Option<String>,
}

/// The gateway method a tool maps to, and the command when there is one.
///
/// Returned rather than applied so the caller can log the translation: an
/// audit line saying "Bash → exec.run" is the one that makes an approval
/// prompt legible months later.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapped {
    pub method: String,
    pub command: Option<String>,
    pub resource: Option<String>,
}

/// Translate a tool call into the vocabulary every door shares.
pub fn map_tool(name: &str, arguments: &Value) -> Mapped {
    let lowered = name.to_ascii_lowercase();
    let text = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|key| arguments.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    };

    let (method, command, resource) = match lowered.as_str() {
        "bash" | "shell" | "exec" | "run_command" | "local_shell" | "terminal" => (
            "exec.run",
            text(&["command", "cmd", "script"]),
            None,
        ),
        "read" | "read_file" | "cat" | "view" => {
            ("brain.read", None, text(&["path", "file", "file_path"]))
        }
        "write" | "write_file" | "edit" | "edit_file" | "apply_patch" | "str_replace" => (
            "brain.write",
            None,
            text(&["path", "file", "file_path"]),
        ),
        "glob" | "grep" | "search" | "list_dir" | "ls" => {
            ("brain.list", None, text(&["path", "pattern"]))
        }
        "webfetch" | "web_search" | "fetch" => ("brain.fetch", None, text(&["url"])),
        _ => (
            // Unknown tools are mutations. The gateway's own classification
            // treats an unrecognised method as writing rather than reading,
            // and a bridge that guessed "probably harmless" would be the hole
            // every future tool walks through.
            "brain.tool",
            text(&["command", "cmd"]),
            text(&["path", "file", "file_path", "url"]),
        ),
    };
    Mapped {
        method: method.to_string(),
        command,
        resource,
    }
}

/// Run one tool request past the gateway.
///
/// Returns `None` when the event is not a tool request — a caller that pipes
/// its whole event stream through here should get nothing for the rest, not a
/// verdict about a sentence the model said.
pub fn admit_tool(
    event: &BrainEvent,
    caller: &Caller,
    policy: &dyn PolicySource,
) -> Option<Passage> {
    let BrainEvent::ToolRequested {
        name, arguments, ..
    } = event
    else {
        return None;
    };

    // A way around the provider is refused here rather than classified and
    // asked about. Approving "run this shell command" would be approving the
    // wrong question: the user would be saying yes to a command, and what
    // would actually happen is a browser with no lease and no trail.
    if let Some(detour) = crate::routing::detour_in_tool(name, arguments) {
        // An exception, if this workspace was given one. Checked after the
        // detour is recognised so the trail says what would have been refused
        // and whose authority let it through — an exception that leaves no
        // trace is indistinguishable from a hole.
        if let Some(grant) =
            crate::routing::exception_for(caller.actor.as_deref(), caller.task_id.as_deref())
        {
            crate::audit_store::append_correlated(
                &serde_json::json!({
                    "at": chrono::Utc::now().to_rfc3339(),
                    "event": "routing.exception_used",
                    "detour": detour.kind,
                    "matched": detour.matched,
                }),
                &crate::audit_store::Correlation {
                    grant_id: Some(grant),
                    task_id: caller.task_id.clone(),
                    state: Some("allowed_by_exception".into()),
                    ..Default::default()
                },
            );
            // Falls through to the ordinary judgement: an exception says the
            // command is not refused *for being automation*, not that it is
            // exempt from everything else.
        } else {
            return Some(Passage {
                verdict: unterm_gateway::Verdict::deny(
                    unterm_gateway::Code::PolicyBlockedPattern,
                    detour.to_string(),
                    unterm_gateway::Risk::Destructive,
                ),
                outcome: Outcome::Refuse,
            });
        }
    }

    let mapped = map_tool(name, arguments);

    // Governed work must arrive with a scope. A turn that carries an upstream
    // task but no workspace is a managed call that lost its boundary
    // somewhere; refusing is the only safe reading. A turn with no task at all
    // is the user's own terminal, which is not what this rule is about.
    let governed = caller.task_id.is_some();
    let changes_something = mapped.method != "brain.read" && mapped.method != "brain.list";
    if governed && caller.workspace_id.is_none() && changes_something {
        return Some(Passage {
            verdict: unterm_gateway::Verdict::deny(
                unterm_gateway::Code::PolicyBlockedPattern,
                format!(
                    "{} changes something and this turn has no path scope; read-only work is still allowed",
                    mapped.method
                ),
                unterm_gateway::Risk::Destructive,
            ),
            outcome: Outcome::Refuse,
        });
    }

    // Where the shell is now, not where it started.
    if let (Some(workspace), Some(cwd)) = (caller.workspace_id.as_deref(), caller.cwd.as_deref()) {
        if changes_something {
            match crate::workspace_scope::check_drift(
                workspace,
                cwd,
                crate::workspace_scope::DriftPolicy::configured(),
            ) {
                Ok(crate::workspace_scope::Drift::Refused { cwd, reason }) => {
                    return Some(Passage {
                        verdict: unterm_gateway::Verdict::deny(
                            unterm_gateway::Code::PolicyBlockedPattern,
                            format!(
                                "this session's working directory is {cwd}, outside the workspace: {reason}"
                            ),
                            unterm_gateway::Risk::Destructive,
                        ),
                        outcome: Outcome::Refuse,
                    });
                }
                Ok(crate::workspace_scope::Drift::Marked { cwd }) => {
                    // Permissive, never silent.
                    crate::audit_store::append_correlated(
                        &serde_json::json!({
                            "at": chrono::Utc::now().to_rfc3339(),
                            "event": "scope.drift_allowed",
                            "cwd": cwd,
                        }),
                        &crate::audit_store::Correlation {
                            workspace_id: Some(workspace.to_string()),
                            task_id: caller.task_id.clone(),
                            state: Some("marked".into()),
                            ..Default::default()
                        },
                    );
                }
                Ok(crate::workspace_scope::Drift::Inside) => {}
                Err(error) => {
                    return Some(Passage {
                        verdict: unterm_gateway::Verdict::deny(
                            unterm_gateway::Code::PolicyBlockedPattern,
                            format!("this turn's workspace could not be checked: {error}"),
                            unterm_gateway::Risk::Destructive,
                        ),
                        outcome: Outcome::Refuse,
                    });
                }
            }
        }
    }

    // Redirections write where no method argument ever names.
    if let (Some(workspace), Some(command)) =
        (caller.workspace_id.as_deref(), mapped.command.as_deref())
    {
        for target in crate::path_scope::write_targets_in(command) {
            let decision =
                crate::workspace_scope::check(workspace, crate::path_scope::PathAccess::Write, &target);
            let refused = match decision {
                Ok(decision) => (!decision.allowed).then(|| decision.reason),
                Err(error) => Some(error.to_string()),
            };
            if let Some(reason) = refused {
                return Some(Passage {
                    verdict: unterm_gateway::Verdict::deny(
                        unterm_gateway::Code::PolicyBlockedPattern,
                        format!("this command writes to {target}, which is outside this workspace: {reason}"),
                        unterm_gateway::Risk::Destructive,
                    ),
                    outcome: Outcome::Refuse,
                });
            }
        }
    }

    if let (Some(workspace), Some(path)) = (caller.workspace_id.as_deref(), mapped.resource.as_deref())
    {
        // Only paths. A `brain.fetch` resource is a URL, and a URL is not
        // something a filesystem scope has an opinion about.
        if mapped.method != "brain.fetch" {
            match crate::workspace_scope::check(workspace, access_for(&mapped.method), path) {
                Ok(decision) if !decision.allowed => {
                    return Some(Passage {
                        verdict: unterm_gateway::Verdict::deny(
                            unterm_gateway::Code::PolicyBlockedPattern,
                            format!(
                                "{path} is outside this workspace ({}). {}",
                                decision.code, decision.reason
                            ),
                            unterm_gateway::Risk::Destructive,
                        ),
                        outcome: Outcome::Refuse,
                    });
                }
                Ok(_) => {}
                // A workspace that cannot be read is not permission to ignore
                // it. Refusing is the only safe reading of "the boundary is
                // unavailable".
                Err(error) => {
                    return Some(Passage {
                        verdict: unterm_gateway::Verdict::deny(
                            unterm_gateway::Code::PolicyBlockedPattern,
                            format!("this turn's workspace could not be checked: {error}"),
                            unterm_gateway::Risk::Destructive,
                        ),
                        outcome: Outcome::Refuse,
                    });
                }
            }
        }
    }

    let mut context = ActionContext::new(mapped.method)
        // Recorded, not judged on: the verdict is the same through every
        // door, but an audit trail that cannot say a model asked is missing
        // the thing people want to know afterwards.
        .entry(Entry::Brain);
    context.command = mapped.command;
    context.resource = mapped.resource;
    context.actor = caller.actor.clone();
    context.task_id = caller.task_id.clone();
    Some(admit(&context, policy))
}

/// Which access a mapped method needs from a workspace.
fn access_for(method: &str) -> crate::path_scope::PathAccess {
    match method {
        "brain.write" => crate::path_scope::PathAccess::Write,
        _ => crate::path_scope::PathAccess::Read,
    }
}

/// Whether the caller may go ahead and run the tool now.
pub fn may_proceed(passage: &Passage) -> bool {
    matches!(passage.outcome, Outcome::Proceed { .. })
}

/// The policy a brain runs under when the caller has not configured one.
pub fn default_policy() -> SettingsPolicy {
    SettingsPolicy::off()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::fleet_store;
    use serde_json::json;
    use unterm_gateway::{Code, Risk};

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    fn request(name: &str, arguments: Value) -> BrainEvent {
        BrainEvent::ToolRequested {
            call_id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn the_shell_a_model_asks_for_is_the_shell_the_policy_judges() {
        let _dir = isolate();
        let policy = SettingsPolicy::new(true, vec!["rm -rf".to_string()], Vec::new());
        // Every CLI has its own name for the same tool; all of them must land
        // on the command the user's blocklist is written against.
        for name in ["Bash", "shell", "local_shell", "run_command"] {
            let passage = admit_tool(
                &request(name, json!({"command": "rm -rf /"})),
                &Caller::default(),
                &policy,
            )
            .expect("a tool request");
            assert_eq!(
                passage.outcome,
                Outcome::Refuse,
                "{name} escaped the blocklist"
            );
            assert_eq!(passage.verdict.code, Code::PolicyBlockedPattern);
        }
    }

    #[test]
    fn a_tool_nobody_has_heard_of_is_not_thereby_safe() {
        // The hole this closes: a new tool name appearing in a CLI update and
        // being waved through because the bridge did not recognise it.
        let _dir = isolate();
        let passage = admit_tool(
            &request("some_new_tool", json!({"path": "/etc/hosts"})),
            &Caller::default(),
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(passage.verdict.risk, Risk::Read);
    }

    #[test]
    fn reading_a_file_does_not_interrupt_anybody() {
        let _dir = isolate();
        let passage = admit_tool(
            &request("Read", json!({"file_path": "/tmp/x"})),
            &Caller::default(),
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(passage.verdict.risk, Risk::Read);
        assert!(may_proceed(&passage));
        assert!(fleet_store::tasks()
            .unwrap()
            .pending_approvals()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn what_the_model_said_is_not_a_tool_request() {
        let _dir = isolate();
        for event in [
            BrainEvent::Text { text: "rm -rf /".into() },
            BrainEvent::Reasoning {
                text: "I could rm -rf /".into(),
            },
            BrainEvent::ToolResult {
                call_id: "c1".into(),
                ok: true,
                output: Some("done".into()),
            },
        ] {
            assert!(
                admit_tool(&event, &Caller::default(), &SettingsPolicy::off()).is_none(),
                "a {} was judged as if it were a tool call",
                event.kind()
            );
        }
    }

    #[test]
    fn the_task_carries_through_so_one_answer_can_cover_a_turn() {
        let _dir = isolate();
        let caller = Caller {
            actor: Some("codex".into()),
            task_id: Some("tsk_1".into()),
            ..Caller::default()
        };
        let passage = admit_tool(
            &request("Bash", json!({"command": "git push"})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        // Whatever the verdict, the audit trail knows who asked and what for.
        assert!(passage.verdict.risk >= Risk::LocalMutation);
        let pending = fleet_store::tasks().unwrap().pending_approvals().unwrap();
        for approval in pending {
            assert_eq!(approval.actor.as_deref(), Some("codex"));
            assert_eq!(approval.task_id.as_deref(), Some("tsk_1"));
        }
    }

    #[test]
    fn a_confined_turn_cannot_read_outside_its_workspace() {
        // Refused before the gateway is consulted: asking the user to approve
        // reading a file the workspace was defined to exclude would be asking
        // them to undo their own boundary one prompt at a time.
        let dir = isolate();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        let inside = dir.path().join("alpha");
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let workspace = crate::workspace_scope::create("alpha", &inside).unwrap();
        let caller = Caller {
            workspace_id: Some(workspace.id.clone()),
            ..Caller::default()
        };

        let passage = admit_tool(
            &request("Read", json!({"file_path": outside.join("secrets").display().to_string()})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(passage.outcome, Outcome::Refuse);
        assert!(passage.verdict.reason.contains("outside this workspace"));

        // And its own root is fine, for reading and for writing.
        for (tool, args) in [
            ("Read", json!({"file_path": inside.join("notes.md").display().to_string()})),
            ("Write", json!({"file_path": inside.join("out.txt").display().to_string()})),
        ] {
            let passage = admit_tool(&request(tool, args), &caller, &SettingsPolicy::off()).unwrap();
            assert_ne!(passage.outcome, Outcome::Refuse, "{tool} in its own root");
        }
    }

    #[test]
    fn a_session_that_wandered_out_cannot_change_anything_there() {
        // `cd` is how a scope stops being true without anybody calling a
        // method. The judgement is remade every call for exactly this.
        let dir = isolate();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::remove_var("UNTERM_SCOPE_DRIFT");
        let inside = dir.path().join("alpha");
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let workspace = crate::workspace_scope::create("alpha", &inside).unwrap();

        let wandered = Caller {
            task_id: Some("tsk_1".into()),
            workspace_id: Some(workspace.id.clone()),
            cwd: Some(outside.display().to_string()),
            ..Caller::default()
        };
        let passage = admit_tool(
            &request("Bash", json!({"command": "rm -f notes.txt"})),
            &wandered,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(passage.outcome, Outcome::Refuse, "{passage:?}");
        assert!(passage.verdict.reason.contains("working directory"));

        // Reading is still fine — the rule is about changing things from
        // somewhere it should not be changing them.
        let read = admit_tool(
            &request("Read", json!({"file_path": inside.join("x").display().to_string()})),
            &wandered,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(read.outcome, Outcome::Refuse);

        // And back inside, work resumes.
        let home = Caller {
            cwd: Some(inside.display().to_string()),
            ..wandered
        };
        let passage = admit_tool(
            &request("Bash", json!({"command": "echo hi"})),
            &home,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(passage.outcome, Outcome::Refuse, "{passage:?}");
    }

    #[test]
    fn a_redirection_out_of_the_workspace_is_refused() {
        let dir = isolate();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        let inside = dir.path().join("alpha");
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let workspace = crate::workspace_scope::create("alpha", &inside).unwrap();
        let caller = Caller {
            task_id: Some("tsk_1".into()),
            workspace_id: Some(workspace.id),
            ..Caller::default()
        };

        let escape = format!("echo secrets > {}/leak.txt", outside.display());
        let passage = admit_tool(
            &request("Bash", json!({"command": escape})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(passage.outcome, Outcome::Refuse, "{passage:?}");
        assert!(passage.verdict.reason.contains("writes to"));

        // Inside its own root, the same shape is fine.
        let fine = format!("echo notes > {}/notes.txt", inside.display());
        let passage = admit_tool(
            &request("Bash", json!({"command": fine})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(passage.outcome, Outcome::Refuse, "{passage:?}");
    }

    #[test]
    fn governed_work_without_a_scope_may_read_but_not_change() {
        // A turn carrying an upstream task but no workspace is a managed call
        // that lost its boundary somewhere.
        let _dir = isolate();
        let caller = Caller {
            task_id: Some("tsk_1".into()),
            ..Caller::default()
        };
        let write = admit_tool(
            &request("Write", json!({"file_path": "/tmp/x"})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(write.outcome, Outcome::Refuse);
        assert!(write.verdict.reason.contains("no path scope"));

        let read = admit_tool(
            &request("Read", json!({"file_path": "/tmp/x"})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(read.outcome, Outcome::Refuse, "read-only work is still allowed");
    }

    #[test]
    fn an_unconfined_turn_is_unchanged() {
        // Most agents drive a terminal the user is sitting at. A workspace is
        // opt-in, and turning it on for everybody would break them.
        let _dir = isolate();
        let passage = admit_tool(
            &request("Read", json!({"file_path": "/etc/hosts"})),
            &Caller::default(),
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_ne!(passage.outcome, Outcome::Refuse);
    }

    #[test]
    fn a_workspace_that_cannot_be_checked_refuses_rather_than_waves_through() {
        let _dir = isolate();
        let caller = Caller {
            workspace_id: Some("wsp_never_created".into()),
            ..Caller::default()
        };
        let passage = admit_tool(
            &request("Read", json!({"file_path": "/tmp/x"})),
            &caller,
            &SettingsPolicy::off(),
        )
        .unwrap();
        assert_eq!(passage.outcome, Outcome::Refuse);
        assert!(passage.verdict.reason.contains("could not be checked"));
    }

    #[test]
    fn an_exception_lets_the_command_through_and_leaves_a_trace() {
        let dir = isolate();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        // A development workspace: the exception is *for* one, and governed
        // work without a scope is refused before this rule is even reached.
        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::workspace_scope::create("dev", &root).unwrap();
        let caller = Caller {
            actor: Some("codex".into()),
            task_id: Some("tsk_dev".into()),
            workspace_id: Some(workspace.id),
            ..Caller::default()
        };
        let request = request("Bash", json!({"command": "npx playwright test"}));

        // Without one: refused.
        assert_eq!(
            admit_tool(&request, &caller, &SettingsPolicy::off())
                .unwrap()
                .outcome,
            Outcome::Refuse
        );

        fleet_store::tasks()
            .unwrap()
            .create_grant(unterm_tasks::NewGrant {
                scope_or_once: Some(unterm_tasks::Scope::Task),
                method: Some(crate::routing::EXCEPTION_METHOD.to_string()),
                actor: Some("codex".into()),
                task_id: Some("tsk_dev".into()),
                resource: None,
                max_risk: Some("destructive".into()),
                ttl_seconds: Some(300),
            })
            .unwrap();

        // With one: judged like any other command rather than refused for
        // being automation.
        let passage = admit_tool(&request, &caller, &SettingsPolicy::off()).unwrap();
        assert_ne!(passage.outcome, Outcome::Refuse, "{passage:?}");

        // And the trail says it happened, and on whose authority.
        let used = crate::audit_store::recent(20)
            .into_iter()
            .find(|entry| entry["event"] == "routing.exception_used")
            .expect("an exception that leaves no trace is indistinguishable from a hole");
        assert_eq!(used["state"], "allowed_by_exception");
        assert!(used["grant_id"].is_string());
    }

    #[test]
    fn a_model_cannot_drive_a_browser_around_the_provider() {
        // M6-05. Refused rather than asked about: approving the shell command
        // would be approving the wrong question.
        let _dir = isolate();
        for command in [
            "curl http://127.0.0.1:9222/json/version",
            "npx playwright open https://example.com",
            "chromium --headless --remote-debugging-port=9222",
        ] {
            let passage = admit_tool(
                &request("Bash", json!({"command": command})),
                &Caller::default(),
                &SettingsPolicy::off(),
            )
            .expect("a tool request");
            assert_eq!(passage.outcome, Outcome::Refuse, "{command}");
            assert!(
                passage.verdict.reason.contains("provider.call"),
                "the refusal does not say what to do instead: {}",
                passage.verdict.reason
            );
        }
        // And nothing was queued for the user to approve, because there is no
        // version of this the user should be asked to allow.
        assert!(fleet_store::tasks()
            .unwrap()
            .pending_approvals()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_brain_crate_cannot_run_a_tool_even_if_someone_wanted_it_to() {
        // The property M4-02 rests on, checked structurally because it is the
        // kind that decays silently: adapters are parsers, and the only place
        // in the brain crate that starts a process is the one that starts the
        // brain itself.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("unterm-brain/src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = std::fs::read_to_string(&path).unwrap();
            if name != "runtime.rs" && source.contains("Command::new") {
                offenders.push(name);
            }
        }
        assert!(
            offenders.is_empty(),
            "these started a process outside the brain's own launcher: {offenders:?}"
        );
    }
}
