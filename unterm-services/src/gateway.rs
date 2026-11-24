//! The one path an action takes.
//!
//! `unterm-gateway` owns the vocabulary and the decision; it deliberately
//! cannot reach a database, because the PTY write path sits below anything
//! that can. This module is where the two halves meet: it supplies the
//! gateway with the user's command policy and their standing grants, runs the
//! decision, and — when the answer is "somebody has to say yes" — records the
//! question somewhere that survives a restart.
//!
//! The order is the contract, and it is the same order for every door:
//!
//! ```text
//! classify → scope → policy → grants → approval → (invoke) → audit
//! ```
//!
//! Each stage may only refuse or defer. None of them can turn a refusal back
//! into an allow, which is what makes the sequence readable as a whole story
//! rather than a pile of special cases.

use crate::cockpit::fleet_store;
use unterm_gateway::{ActionContext, Code, Entry, GrantSource, PolicySource, Risk, Verdict};
use unterm_tasks::{Ask, NewGrant, Scope};

/// The user's command policy, read from settings.
///
/// Its absence is not permission: a policy that fails to load leaves the
/// blocklist empty, so this reports "off" rather than pretending to enforce
/// something it could not read.
pub struct SettingsPolicy {
    enabled: bool,
    blocked: Vec<String>,
    allowed: Vec<String>,
}

impl SettingsPolicy {
    pub fn new(enabled: bool, blocked: Vec<String>, allowed: Vec<String>) -> Self {
        Self {
            enabled,
            blocked,
            allowed,
        }
    }

    /// A policy that forbids nothing, for callers that have none configured.
    pub fn off() -> Self {
        Self {
            enabled: false,
            blocked: Vec::new(),
            allowed: Vec::new(),
        }
    }
}

impl PolicySource for SettingsPolicy {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn blocked_patterns(&self) -> Vec<String> {
        self.blocked.clone()
    }
    fn allowed_patterns(&self) -> Vec<String> {
        self.allowed.clone()
    }
}

/// Standing grants, read from the durable store.
pub struct StoredGrants;

fn ask_from(context: &ActionContext, risk: Risk) -> Ask {
    Ask {
        method: context.method.clone(),
        actor: context.actor.clone(),
        task_id: context.task_id.clone(),
        resource: context.resource.clone(),
        risk_rank: unterm_tasks::approval::risk_rank(risk.as_str()),
    }
}

impl GrantSource for StoredGrants {
    fn covers(&self, context: &ActionContext, risk: Risk) -> bool {
        grant_covering(context, risk).is_some()
    }
}

/// Which grant covers this, if any. Returned rather than a bare bool so the
/// caller can record *which* permission let an action through — revocation
/// has no way to find the work it must stop otherwise.
pub fn grant_covering(context: &ActionContext, risk: Risk) -> Option<String> {
    let store = fleet_store::tasks()?;
    store
        .covering_grant(&ask_from(context, risk))
        .ok()
        .flatten()
        .map(|grant| grant.id)
}

/// What the caller must do next.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Go ahead. `authorised_by` names the grant that allowed it, when one
    /// did, so the step can be stamped and revocation can find it later.
    Proceed { authorised_by: Option<String> },
    /// Somebody has to answer first. The question is already recorded, and
    /// its id is how the caller waits for or cancels it.
    AwaitApproval { approval_id: String },
    /// No.
    Refuse,
}

/// The decision, and the recorded question when one is needed.
#[derive(Clone, Debug)]
pub struct Passage {
    pub verdict: Verdict,
    pub outcome: Outcome,
}

/// Run an action through the gateway.
///
/// A dry run stops before anything is written down: asking what *would*
/// happen must not leave a question in the user's approval queue, or a UI
/// that previews an action would fill their screen with prompts nobody asked
/// for.
pub fn admit(context: &ActionContext, policy: &dyn PolicySource) -> Passage {
    let verdict = unterm_gateway::decide(context, policy, &StoredGrants);

    if !verdict.allowed && verdict.needs_approval && !context.dry_run {
        let approval_id = record_question(context, &verdict);
        if let Some(approval_id) = approval_id {
            return Passage {
                verdict,
                outcome: Outcome::AwaitApproval { approval_id },
            };
        }
        // Nowhere to record the question means nobody can answer it. Refusing
        // is the only honest end: proceeding would perform a destructive
        // action that was never approved, and pretending to wait would hang.
        return Passage {
            verdict,
            outcome: Outcome::Refuse,
        };
    }

    let outcome = if verdict.allowed {
        Outcome::Proceed {
            authorised_by: (verdict.code == Code::AllowedByGrant)
                .then(|| grant_covering(context, verdict.risk))
                .flatten(),
        }
    } else {
        Outcome::Refuse
    };
    Passage { verdict, outcome }
}

fn record_question(context: &ActionContext, verdict: &Verdict) -> Option<String> {
    let store = fleet_store::tasks()?;
    let ask = ask_from(context, verdict.risk);
    store
        .request_approval(
            &context.method,
            verdict.risk.as_str(),
            &ask,
            context.command.as_deref(),
            // Questions do not wait forever. An approval nobody answers ends
            // as `Expired`, which a caller can tell apart from a refusal.
            Some(APPROVAL_TTL_SECONDS),
        )
        .ok()
        .map(|approval| approval.id)
}

/// How long a question waits before nobody answering becomes an answer.
pub const APPROVAL_TTL_SECONDS: i64 = 300;

/// Answer a question, optionally remembering the answer.
pub fn answer(
    approval_id: &str,
    allowed: bool,
    decided_by: &str,
    remember: Option<Scope>,
    context: &ActionContext,
    risk: Risk,
) -> anyhow::Result<()> {
    let store = fleet_store::tasks()
        .ok_or_else(|| anyhow::anyhow!("there is no approval store to answer into"))?;
    let remember = remember.map(|scope| NewGrant {
        // Secrets, money and destruction are never remembered past the call.
        // Narrowed here, at the moment the permission is written, rather than
        // only where it is matched: a grant that is stored but can never
        // answer is worse than no grant, because the user was told they had
        // given one. What they get instead is the yes they actually meant --
        // this action, now, once.
        scope_or_once: Some(if risk.allows_standing_grant() {
            scope
        } else {
            Scope::Once
        }),
        // A remembered answer is about the action that was asked, not about
        // everything: the narrowest grant that satisfies the user's intent is
        // the one to create.
        method: Some(context.method.clone()),
        actor: context.actor.clone(),
        task_id: context.task_id.clone(),
        resource: context.resource.clone(),
        max_risk: Some(risk.as_str().to_string()),
        ttl_seconds: None,
    });
    store.decide_approval(approval_id, allowed, decided_by, remember)?;
    Ok(())
}

/// The questions waiting for a human.
pub fn pending() -> Vec<unterm_tasks::Approval> {
    fleet_store::tasks()
        .and_then(|store| store.pending_approvals().ok())
        .unwrap_or_default()
}

/// Answer a question knowing only its id.
///
/// [`answer`] needs the context the question came from, which the caller who
/// asked still has. Whoever is *answering* — a settings page listing what is
/// waiting — does not, and should not have to reconstruct it: the recorded
/// approval already says what was asked, by whom and about what, and a grant
/// derived from anything else would be a grant for a different question.
pub fn answer_by_id(
    approval_id: &str,
    allowed: bool,
    decided_by: &str,
    remember: Option<Scope>,
) -> anyhow::Result<unterm_tasks::Approval> {
    let store = fleet_store::tasks()
        .ok_or_else(|| anyhow::anyhow!("there is no approval store to answer into"))?;
    let approval = store
        .approval(approval_id)?
        .ok_or_else(|| anyhow::anyhow!("no such approval: {approval_id}"))?;
    if approval.state != unterm_tasks::ApprovalState::Pending {
        // Answering twice is not an error worth hiding: the second answer
        // would create a second grant for a question already settled.
        anyhow::bail!(
            "{approval_id} was already {}",
            approval.state.as_str()
        );
    }
    // The same narrowing as `answer`, and the more important of the two
    // doors: this is the one a settings page answers through, which is where
    // "always allow" is actually clicked. The risk comes back from the store
    // as text, so it is judged as text -- including text a newer build wrote,
    // which is unrecognised here and therefore treated as strictly as the
    // three named ones.
    let remember = remember.map(|scope| NewGrant {
        scope_or_once: Some(if unterm_tasks::risk_allows_standing_grant(&approval.risk) {
            scope
        } else {
            Scope::Once
        }),
        method: Some(approval.method.clone()),
        actor: approval.actor.clone(),
        task_id: approval.task_id.clone(),
        resource: approval.resource.clone(),
        max_risk: Some(approval.risk.clone()),
        ttl_seconds: None,
    });
    store.decide_approval(approval_id, allowed, decided_by, remember)?;
    store
        .approval(approval_id)?
        .ok_or_else(|| anyhow::anyhow!("the approval vanished while being answered"))
}

/// Describe an entry point for the audit trail.
pub fn describe_entry(entry: Option<Entry>) -> &'static str {
    entry.map(Entry::as_str).unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    #[test]
    fn a_read_goes_straight_through() {
        let _dir = isolate();
        let passage = admit(&ActionContext::new("screen.read"), &SettingsPolicy::off());
        assert!(passage.verdict.allowed);
        assert_eq!(passage.outcome, Outcome::Proceed { authorised_by: None });
        // And it leaves no question behind.
        assert!(fleet_store::tasks()
            .unwrap()
            .pending_approvals()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_destructive_action_records_a_question_somebody_can_answer() {
        let _dir = isolate();
        let context = ActionContext::new("session.destroy")
            .entry(Entry::Mcp)
            .actor("claude");
        let passage = admit(&context, &SettingsPolicy::off());

        let Outcome::AwaitApproval { approval_id } = passage.outcome else {
            panic!("a destructive action was not held for approval: {passage:?}");
        };
        let store = fleet_store::tasks().unwrap();
        let pending = store.pending_approvals().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, approval_id);
        assert_eq!(pending[0].actor.as_deref(), Some("claude"));
        assert_eq!(pending[0].risk, "destructive");
    }

    #[test]
    fn a_dry_run_asks_nobody() {
        let _dir = isolate();
        let context = ActionContext::new("session.destroy").dry_run(true);
        let passage = admit(&context, &SettingsPolicy::off());

        assert!(!passage.verdict.allowed);
        assert!(passage.verdict.needs_approval);
        assert_eq!(passage.outcome, Outcome::Refuse);
        assert!(
            fleet_store::tasks()
                .unwrap()
                .pending_approvals()
                .unwrap()
                .is_empty(),
            "previewing an action must not fill the user's queue with prompts"
        );
    }

    #[test]
    fn answering_and_remembering_lets_the_next_one_through_named() {
        let _dir = isolate();
        let context = ActionContext::new("session.destroy")
            .actor("claude")
            .task("tsk_1");
        let Outcome::AwaitApproval { approval_id } =
            admit(&context, &SettingsPolicy::off()).outcome
        else {
            panic!("expected a question");
        };
        answer(
            &approval_id,
            true,
            "the user",
            Some(Scope::Task),
            &context,
            Risk::Destructive,
        )
        .unwrap();

        // The next identical action is covered, and says which grant did it.
        let passage = admit(&context, &SettingsPolicy::off());
        assert!(passage.verdict.allowed);
        assert_eq!(passage.verdict.code, Code::AllowedByGrant);
        let Outcome::Proceed { authorised_by } = passage.outcome else {
            panic!("a granted action was not allowed to proceed");
        };
        let grant_id = authorised_by.expect("the grant that allowed it must be named");

        // A different task is not covered by a task-scoped answer.
        let elsewhere = ActionContext::new("session.destroy")
            .actor("claude")
            .task("tsk_2");
        assert!(matches!(
            admit(&elsewhere, &SettingsPolicy::off()).outcome,
            Outcome::AwaitApproval { .. }
        ));

        // And revoking puts the question back.
        fleet_store::tasks().unwrap().revoke_grant(&grant_id).unwrap();
        assert!(matches!(
            admit(&context, &SettingsPolicy::off()).outcome,
            Outcome::AwaitApproval { .. }
        ));
    }

    #[test]
    fn policy_refuses_before_anyone_is_asked() {
        let _dir = isolate();
        let policy = SettingsPolicy::new(true, vec!["rm -rf".to_string()], Vec::new());
        let context = ActionContext::new("exec.run").command("rm -rf /");
        let passage = admit(&context, &policy);

        assert_eq!(passage.outcome, Outcome::Refuse);
        assert_eq!(passage.verdict.code, Code::PolicyBlockedPattern);
        assert!(
            fleet_store::tasks()
                .unwrap()
                .pending_approvals()
                .unwrap()
                .is_empty(),
            "a blocked command must not become a question the user could say yes to"
        );
    }

    #[test]
    fn every_door_reaches_the_same_outcome() {
        // M3's gate, at the level callers actually use.
        let _dir = isolate();
        let policy = SettingsPolicy::new(true, vec!["rm -rf".to_string()], Vec::new());
        for door in [
            Entry::Mcp,
            Entry::Cli,
            Entry::Brain,
            Entry::Workflow,
            Entry::Pty,
            Entry::User,
        ] {
            let passage = admit(
                &ActionContext::new("exec.run")
                    .entry(door)
                    .command("rm -rf /"),
                &policy,
            );
            assert_eq!(
                passage.outcome,
                Outcome::Refuse,
                "the {door:?} door reached a different outcome"
            );
            assert_eq!(passage.verdict.code, Code::PolicyBlockedPattern);
        }
    }

    /// "Remember this" means something different for a deletion.
    ///
    /// The user is offered "for this task" and means it; what they may have
    /// is one use. Narrowed where the grant is written rather than refused,
    /// because refusing would lose the yes they did give -- and stored as
    /// `Once` rather than as a task grant that can never answer, so what the
    /// settings page lists is what will actually happen.
    #[test]
    fn a_remembered_yes_to_a_deletion_is_narrowed_to_a_single_use() {
        let _dir = isolate();
        let context = ActionContext::new("session.destroy")
            .actor("claude")
            .task("tsk_1");
        let Outcome::AwaitApproval { approval_id } =
            admit(&context, &SettingsPolicy::off()).outcome
        else {
            panic!("expected a question");
        };
        answer(
            &approval_id,
            true,
            "the user",
            Some(Scope::Task),
            &context,
            Risk::Destructive,
        )
        .unwrap();

        let store = fleet_store::tasks().expect("a store");
        let grants = store.grants().expect("grants");
        let grant = grants
            .iter()
            .find(|grant| grant.method.as_deref() == Some("session.destroy"))
            .expect("the answer was remembered");
        assert_eq!(
            grant.scope,
            Scope::Once,
            "a task-scoped yes to a destructive action must not be stored as one"
        );

        // And a band that may be remembered keeps the scope it was given.
        // `upload.file` is the pair to `session.destroy` here: both are asked
        // about, and only one of them may be answered once and for all.
        let sending = ActionContext::new("upload.file")
            .actor("claude")
            .task("tsk_1");
        let Outcome::AwaitApproval { approval_id } =
            admit(&sending, &SettingsPolicy::off()).outcome
        else {
            panic!("expected a question");
        };
        answer(
            &approval_id,
            true,
            "the user",
            Some(Scope::Task),
            &sending,
            Risk::ExternalSideEffect,
        )
        .unwrap();
        let grants = store.grants().expect("grants");
        let kept = grants
            .iter()
            .find(|grant| grant.method.as_deref() == Some("upload.file"))
            .expect("the answer was remembered");
        assert_eq!(kept.scope, Scope::Task);
    }

}
