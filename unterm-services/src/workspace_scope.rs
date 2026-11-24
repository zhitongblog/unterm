//! Workspaces: named roots that work is confined to, and blind to each other.
//!
//! A note on the word, because this codebase already uses it: `workspace.*`
//! on the MCP surface means a saved pane layout, and has since long before
//! this. These are a different thing with the same name in the plan, so they
//! live under `scope.*` on the wire. Nothing here touches pane layouts.
//!
//! The property that matters — **two workspaces cannot see each other** —
//! cannot be established by checking one path against one root. It is a
//! property of the whole set, so a scope is built by reading every workspace
//! and turning the others into explicit denials. That is why this module
//! exists at all rather than callers assembling a [`PathScope`] themselves:
//! a caller who forgets the other roots gets a scope that looks right and
//! leaks.
//!
//! Nesting is refused at creation. A workspace inside another cannot be
//! isolated from it — the outer one's allow and the inner one's deny describe
//! the same files — and the honest moment to say so is when it is being
//! created, not on the first path that surprises somebody.

use crate::path_scope::{PathAccess, PathScope, PathScopeDecision};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use unterm_tasks::Workspace;

/// Resolve a path the way a workspace root must be stored: absolute, with
/// symlinks and `..` gone.
///
/// Done once, at creation. Resolving on every check would mean a root that
/// can move out from under a scope between two checks — replace a symlinked
/// directory and every future answer changes.
pub fn canonical_root(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    let resolved = path
        .canonicalize()
        .map_err(|error| anyhow!("{} cannot be resolved: {error}", path.display()))?;
    if !resolved.is_dir() {
        return Err(anyhow!("{} is not a directory", resolved.display()));
    }
    Ok(resolved)
}

fn store() -> Result<std::sync::Arc<unterm_tasks::TaskStore>> {
    crate::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

/// Make a workspace out of a directory.
pub fn create(name: &str, path: impl AsRef<Path>) -> Result<Workspace> {
    let root = canonical_root(path)?;
    let store = store()?;
    let existing = store.workspaces()?;

    for other in &existing {
        let other_root = PathBuf::from(&other.root);
        if other_root == root {
            return Err(anyhow!(
                "{} is already the workspace {:?}",
                root.display(),
                other.name
            ));
        }
        // Either direction of nesting is refused, and named: telling somebody
        // "that is inside Foo" is actionable, "invalid path" is not.
        if crate::path_scope::contains(&other_root, &root) {
            return Err(anyhow!(
                "{} is inside the workspace {:?}, which cannot be isolated from it",
                root.display(),
                other.name
            ));
        }
        if crate::path_scope::contains(&root, &other_root) {
            return Err(anyhow!(
                "{} contains the workspace {:?}, which cannot be isolated from it",
                root.display(),
                other.name
            ));
        }
    }

    store.create_workspace(name, &root.to_string_lossy())
}

pub fn list() -> Result<Vec<Workspace>> {
    store()?.workspaces()
}

pub fn get(id: &str) -> Result<Option<Workspace>> {
    store()?.workspace(id)
}

pub fn archive(id: &str) -> Result<bool> {
    store()?.archive_workspace(id)
}

/// The scope one workspace works under.
///
/// Its own root is readable and writable; every *other* workspace root is
/// denied outright, including archived ones. An archived workspace is still
/// somebody's files, and "nobody is working there any more" is not a reason
/// to let a different workspace in.
pub fn scope_for(id: &str) -> Result<PathScope> {
    let all = list()?;
    let mine = all
        .iter()
        .find(|workspace| workspace.id == id)
        .ok_or_else(|| anyhow!("no such workspace: {id}"))?;
    Ok(PathScope {
        read_paths: vec![PathBuf::from(&mine.root)],
        write_paths: vec![PathBuf::from(&mine.root)],
        deny_paths: all
            .iter()
            .filter(|workspace| workspace.id != id)
            .map(|workspace| PathBuf::from(&workspace.root))
            .collect(),
    })
}

/// Answer whether a workspace may touch a path.
pub fn check(id: &str, access: PathAccess, path: impl AsRef<Path>) -> Result<PathScopeDecision> {
    Ok(scope_for(id)?.check(access, path))
}

/// What to do when a session's working directory has left the scope.
///
/// A shell can `cd` anywhere; the scope was checked when the session started
/// and that answer stops being true the moment it does. The policy is
/// configurable because both answers are defensible — refusing is safe and
/// surprising, marking is honest and permissive — and a product that picks
/// one for everybody will be wrong for half of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftPolicy {
    /// Refuse side effects until the session comes back. The default: a
    /// command run from outside the scope writes relative paths outside it,
    /// and "it was inside when we started" is not a property anyone can rely
    /// on.
    Refuse,
    /// Allow, and record that it happened outside. For workspaces that are a
    /// starting point rather than a fence.
    Mark,
}

impl DriftPolicy {
    /// What the user configured, defaulting to the safe answer.
    pub fn configured() -> Self {
        match std::env::var("UNTERM_SCOPE_DRIFT").as_deref() {
            Ok("mark") => DriftPolicy::Mark,
            _ => DriftPolicy::Refuse,
        }
    }
}

/// What a drifted session may do.
#[derive(Clone, Debug, PartialEq)]
pub enum Drift {
    /// The working directory is still inside.
    Inside,
    /// It has left, and the policy says stop.
    Refused { cwd: String, reason: String },
    /// It has left, and the policy says carry on — but say so.
    Marked { cwd: String },
}

/// Judge a session's current working directory, not the one it started in.
///
/// **The judgement is not cached.** Re-resolving on every call is the whole
/// point: a decision made when the session started answers a question about a
/// directory the session may have left twenty commands ago.
pub fn check_drift(workspace: &str, cwd: &str, policy: DriftPolicy) -> Result<Drift> {
    if cwd.trim().is_empty() {
        // A shell that has not reported its directory is not evidence that it
        // is somewhere allowed.
        return Ok(match policy {
            DriftPolicy::Refuse => Drift::Refused {
                cwd: String::new(),
                reason: "this session has not reported a working directory".into(),
            },
            DriftPolicy::Mark => Drift::Marked { cwd: String::new() },
        });
    }
    let decision = check(workspace, PathAccess::Read, cwd)?;
    if decision.allowed {
        return Ok(Drift::Inside);
    }
    Ok(match policy {
        DriftPolicy::Refuse => Drift::Refused {
            cwd: decision.resolved_path.unwrap_or_else(|| cwd.to_string()),
            reason: decision.reason,
        },
        DriftPolicy::Mark => Drift::Marked {
            cwd: decision.resolved_path.unwrap_or_else(|| cwd.to_string()),
        },
    })
}

/// Which workspace a path belongs to, if any.
///
/// Used to attribute work rather than to authorise it: a path can only be in
/// one workspace, because nesting was refused when they were created.
pub fn owning(path: impl AsRef<Path>) -> Result<Option<Workspace>> {
    let path = path.as_ref();
    Ok(list()?.into_iter().find(|workspace| {
        crate::path_scope::contains(Path::new(&workspace.root), path)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    fn dir(under: &Path, name: &str) -> PathBuf {
        let path = under.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_workspace_can_work_in_its_own_root() {
        let store = isolate();
        let root = dir(store.path(), "alpha");
        let workspace = create("alpha", &root).unwrap();

        for access in [PathAccess::Read, PathAccess::Write] {
            let decision = check(&workspace.id, access, root.join("notes.txt")).unwrap();
            assert!(decision.allowed, "{decision:?}");
        }
    }

    #[test]
    fn two_workspaces_cannot_see_each_other() {
        // M6's gate. Neither direction, read or write.
        let store = isolate();
        let alpha_root = dir(store.path(), "alpha");
        let bravo_root = dir(store.path(), "bravo");
        let alpha = create("alpha", &alpha_root).unwrap();
        let bravo = create("bravo", &bravo_root).unwrap();

        for (workspace, forbidden) in [(&alpha, &bravo_root), (&bravo, &alpha_root)] {
            for access in [PathAccess::Read, PathAccess::Write] {
                let decision = check(&workspace.id, access, forbidden.join("secret")).unwrap();
                assert!(
                    !decision.allowed,
                    "{} reached {}: {decision:?}",
                    workspace.name,
                    forbidden.display()
                );
                assert_eq!(decision.code, "path_scope_denied_path");
            }
        }
    }

    #[test]
    fn a_workspace_that_appears_later_is_denied_too() {
        // The scope is built from the set each time, so a workspace created
        // after the first one is not a hole in it. A scope cached at creation
        // would be.
        let store = isolate();
        let alpha = create("alpha", dir(store.path(), "alpha")).unwrap();
        let bravo_root = dir(store.path(), "bravo");
        // Before it is a workspace the directory is merely outside alpha…
        let before = check(&alpha.id, PathAccess::Read, bravo_root.join("x")).unwrap();
        assert_eq!(before.code, "path_scope_read_outside_scope");

        // …and afterwards it is somebody else's, which is a different answer
        // for a different reason.
        create("bravo", &bravo_root).unwrap();
        let after = check(&alpha.id, PathAccess::Read, bravo_root.join("x")).unwrap();
        assert!(!after.allowed);
        assert_eq!(after.code, "path_scope_denied_path");
    }

    #[test]
    fn an_archived_workspace_is_still_off_limits() {
        // Nobody is working there any more; the files are still somebody's.
        let store = isolate();
        let alpha = create("alpha", dir(store.path(), "alpha")).unwrap();
        let bravo_root = dir(store.path(), "bravo");
        let bravo = create("bravo", &bravo_root).unwrap();
        assert!(archive(&bravo.id).unwrap());

        assert!(!check(&alpha.id, PathAccess::Read, bravo_root.join("x"))
            .unwrap()
            .allowed);
    }

    #[test]
    fn nesting_is_refused_when_it_is_created_not_when_it_surprises_somebody() {
        let store = isolate();
        let outer = dir(store.path(), "outer");
        create("outer", &outer).unwrap();

        let inner = dir(&outer, "inner");
        let error = create("inner", &inner).unwrap_err().to_string();
        assert!(error.contains("inside the workspace"), "{error}");
        assert!(error.contains("outer"), "the message does not say which: {error}");

        // And the other direction: a new workspace that would swallow one.
        drop(store);
        let store2 = isolate();
        let inner = dir(store2.path(), "outer/inner");
        create("inner", &inner).unwrap();
        let error = create("outer", store2.path().join("outer"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("contains the workspace"), "{error}");
    }

    #[test]
    fn the_same_directory_twice_is_refused() {
        let store = isolate();
        let root = dir(store.path(), "alpha");
        create("alpha", &root).unwrap();
        let error = create("alpha again", &root).unwrap_err().to_string();
        assert!(error.contains("already the workspace"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_out_of_a_workspace_does_not_leave_it() {
        let store = isolate();
        let alpha_root = dir(store.path(), "alpha");
        let bravo_root = dir(store.path(), "bravo");
        let alpha = create("alpha", &alpha_root).unwrap();
        create("bravo", &bravo_root).unwrap();
        std::os::unix::fs::symlink(&bravo_root, alpha_root.join("shortcut")).unwrap();

        let decision = check(&alpha.id, PathAccess::Read, alpha_root.join("shortcut")).unwrap();
        assert!(
            !decision.allowed,
            "a symlink walked out of the workspace: {decision:?}"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_yet_is_still_placed() {
        // Writing a new file is the common case; refusing every path that is
        // not there yet would make workspaces useless for creating anything.
        let store = isolate();
        let root = dir(store.path(), "alpha");
        let workspace = create("alpha", &root).unwrap();
        let decision = check(
            &workspace.id,
            PathAccess::Write,
            root.join("not/created/yet.txt"),
        )
        .unwrap();
        assert!(decision.allowed, "{decision:?}");
    }

    #[test]
    fn a_root_that_is_not_a_directory_is_refused() {
        let store = isolate();
        let file = store.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let error = create("file", &file).unwrap_err().to_string();
        assert!(error.contains("not a directory"), "{error}");

        let error = create("missing", store.path().join("nowhere"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot be resolved"), "{error}");
    }

    #[test]
    fn a_session_that_cd_ed_out_stops_being_inside() {
        // The answer from when the session started is about a directory it
        // may have left twenty commands ago.
        let store = isolate();
        let root = dir(store.path(), "alpha");
        let elsewhere = dir(store.path(), "elsewhere");
        let workspace = create("alpha", &root).unwrap();

        assert_eq!(
            check_drift(&workspace.id, &root.display().to_string(), DriftPolicy::Refuse).unwrap(),
            Drift::Inside
        );

        let drifted = check_drift(
            &workspace.id,
            &elsewhere.display().to_string(),
            DriftPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(drifted, Drift::Refused { .. }), "{drifted:?}");

        // The other policy is permissive but not silent.
        let marked = check_drift(
            &workspace.id,
            &elsewhere.display().to_string(),
            DriftPolicy::Mark,
        )
        .unwrap();
        assert!(matches!(marked, Drift::Marked { .. }), "{marked:?}");
    }

    #[test]
    fn a_shell_that_has_not_said_where_it_is_is_not_assumed_to_be_inside() {
        let store = isolate();
        let workspace = create("alpha", dir(store.path(), "alpha")).unwrap();
        let unknown = check_drift(&workspace.id, "", DriftPolicy::Refuse).unwrap();
        assert!(matches!(unknown, Drift::Refused { .. }), "{unknown:?}");
    }

    #[test]
    fn the_safe_policy_is_the_default() {
        std::env::remove_var("UNTERM_SCOPE_DRIFT");
        assert_eq!(DriftPolicy::configured(), DriftPolicy::Refuse);
        std::env::set_var("UNTERM_SCOPE_DRIFT", "mark");
        assert_eq!(DriftPolicy::configured(), DriftPolicy::Mark);
        std::env::remove_var("UNTERM_SCOPE_DRIFT");
    }

    #[test]
    fn a_path_is_attributed_to_the_workspace_it_is_in() {
        let store = isolate();
        let alpha_root = dir(store.path(), "alpha");
        let alpha = create("alpha", &alpha_root).unwrap();
        create("bravo", dir(store.path(), "bravo")).unwrap();

        assert_eq!(
            owning(alpha_root.join("deep/file.txt")).unwrap().map(|w| w.id),
            Some(alpha.id)
        );
        assert!(owning(store.path().join("elsewhere")).unwrap().is_none());
    }
}
