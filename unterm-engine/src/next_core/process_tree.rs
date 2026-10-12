use super::ProcessTreeSnapshot;
use procinfo::LocalProcessInfo;
use std::path::Path;

#[derive(Clone, Debug)]
pub(super) struct ProcessNodeSummary {
    pub(super) pid: u32,
    pub(super) name: String,
    pub(super) cwd: Option<String>,
    pub(super) argv: Vec<String>,
    start_time: u64,
    child_count: usize,
    pub(super) detected_agent: Option<String>,
}

/// Where a pane is now, best source first.
///
/// A shell that reports OSC 7 is authoritative and costs nothing, but in
/// Unterm that escape never arrives: macOS only installs zsh's
/// `update_terminal_cwd` hook by sourcing `/etc/zshrc_$TERM_PROGRAM`, and a
/// GUI launched from Finder inherits no `TERM_PROGRAM` at all -- the same
/// empty-environment trap that once left `TERM` unset and turned colour off.
///
/// So the process walk is not a rare fallback, it is the normal path, and it
/// has to run on every read. It used to run only while the recorded cwd was
/// still empty, which meant a pane *opened in* a directory -- it starts with
/// one recorded -- could never report a `cd` again. It showed the folder it
/// was opened in for the rest of its life.
pub(super) fn resolve_cwd(
    reported: Option<String>,
    existing: Option<String>,
    root_pid: Option<u32>,
) -> Option<String> {
    if reported.is_some() {
        return reported;
    }
    // Ask the one process we care about, rather than building the whole
    // process tree and walking it three times over. This runs on every
    // snapshot now, and the tree walk is recursive: on Windows, where the
    // main thread gets a 1 MiB stack, doing it per snapshot overflowed it
    // outright. The pane's own shell is also the honest answer to "which
    // directory is this pane in" -- a long-running foreground program does
    // not move the shell.
    let probed = root_pid
        .and_then(LocalProcessInfo::current_working_dir)
        .as_deref()
        .and_then(path_to_non_empty_string);
    choose_cwd(reported, probed, existing)
}

/// Keep the last known directory when nothing can be read now, rather than
/// blanking the pane's label because one probe happened to fail.
fn choose_cwd(
    reported: Option<String>,
    probed: Option<String>,
    existing: Option<String>,
) -> Option<String> {
    reported.or(probed).or(existing)
}

pub(super) fn snapshot(
    root_pid: Option<u32>,
    fallback_process: &str,
) -> Option<ProcessTreeSnapshot> {
    let Some(pid) = root_pid else {
        return Some(fallback(None, fallback_process));
    };
    let Some(root) = LocalProcessInfo::with_root_pid(pid) else {
        return Some(fallback(Some(pid), fallback_process));
    };
    Some(snapshot_from_root(&root))
}

fn fallback(root_pid: Option<u32>, fallback_process: &str) -> ProcessTreeSnapshot {
    ProcessTreeSnapshot {
        root_pid,
        root_process: fallback_process.to_string(),
        root_cwd: None,
        foreground_pid: root_pid,
        foreground_process: fallback_process.to_string(),
        foreground_cwd: None,
        foreground_argv: Vec::new(),
        child_count: 0,
        detected_agent: detect_known_agent_name(fallback_process).map(str::to_string),
    }
}

pub(super) fn snapshot_from_root(root: &LocalProcessInfo) -> ProcessTreeSnapshot {
    let foreground = foreground_process_summary(root);
    ProcessTreeSnapshot {
        root_pid: Some(root.pid),
        root_process: root.name.clone(),
        root_cwd: path_to_non_empty_string(&root.cwd),
        foreground_pid: Some(foreground.pid),
        foreground_process: foreground.name,
        foreground_cwd: foreground.cwd,
        foreground_argv: foreground.argv,
        child_count: count_descendants(root),
        detected_agent: detect_agent_in_process_tree(root),
    }
}

pub(super) fn foreground_process_summary(root: &LocalProcessInfo) -> ProcessNodeSummary {
    let mut best = ProcessNodeSummary {
        pid: root.pid,
        name: root.name.clone(),
        cwd: path_to_non_empty_string(&root.cwd),
        argv: root.argv.clone(),
        start_time: root.start_time,
        child_count: count_descendants(root),
        detected_agent: detect_known_agent_name(&root.name)
            .or_else(|| {
                root.argv
                    .first()
                    .and_then(|arg| detect_known_agent_name(arg))
            })
            .map(str::to_string),
    };
    for child in root.children.values() {
        let candidate = foreground_process_summary(child);
        let candidate_score = (
            candidate.detected_agent.is_some(),
            candidate.start_time,
            candidate.child_count,
        );
        let best_score = (
            best.detected_agent.is_some(),
            best.start_time,
            best.child_count,
        );
        if candidate_score > best_score {
            best = candidate;
        }
    }
    best
}

fn path_to_non_empty_string(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    (!text.is_empty()).then(|| text.into_owned())
}

fn count_descendants(root: &LocalProcessInfo) -> usize {
    root.children
        .values()
        .map(|child| 1 + count_descendants(child))
        .sum()
}

pub(super) fn detect_agent_in_process_tree(root: &LocalProcessInfo) -> Option<String> {
    detect_known_agent_name(&root.name)
        .or_else(|| {
            root.argv
                .first()
                .and_then(|arg| detect_known_agent_name(arg))
        })
        .map(str::to_string)
        .or_else(|| {
            root.children
                .values()
                .find_map(detect_agent_in_process_tree)
        })
}

fn detect_known_agent_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let bare = std::path::Path::new(&lower)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&lower);
    match bare {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "kimi" | "kimi-code" => Some("kimi"),
        "aider" => Some("aider"),
        "opencode" => Some("opencode"),
        "trae" | "trae-cli" | "trae_agent" | "trae-agent" => Some("trae"),
        "zcode" | "z-code" | "z code" => Some("zcode"),
        "cursor-agent" | "cursoragent" => Some("cursor-agent"),
        "agy" | "antigravity" => Some("antigravity"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::choose_cwd;

    fn s(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn a_shell_that_reports_its_directory_is_believed() {
        assert_eq!(
            choose_cwd(s("/now"), s("/probed"), s("/stale")),
            s("/now")
        );
    }

    /// The regression: a pane opened *in* a directory already has one
    /// recorded, and used to be pinned to it for life because the process
    /// walk only ran while the record was empty.
    #[test]
    fn a_recorded_directory_does_not_outrank_a_fresh_probe() {
        assert_eq!(choose_cwd(None, s("/now"), s("/opened-in")), s("/now"));
    }

    #[test]
    fn a_failed_probe_keeps_the_last_known_directory() {
        assert_eq!(choose_cwd(None, None, s("/last-known")), s("/last-known"));
    }

    #[test]
    fn nothing_known_reports_nothing() {
        assert_eq!(choose_cwd(None, None, None), None);
    }

    use super::*;
    use std::path::PathBuf;

    fn process_info_for_test(
        pid: u32,
        ppid: u32,
        name: &str,
        argv: Vec<String>,
        start_time: u64,
        children: Vec<LocalProcessInfo>,
    ) -> LocalProcessInfo {
        LocalProcessInfo {
            pid,
            ppid,
            name: name.to_string(),
            executable: PathBuf::from(name),
            argv,
            cwd: PathBuf::from(format!("C:\\work\\{name}-{pid}")),
            status: procinfo::LocalProcessStatus::Run,
            start_time,
            #[cfg(windows)]
            console: 0,
            children: children
                .into_iter()
                .map(|child| (child.pid, child))
                .collect(),
        }
    }

    #[test]
    fn summary_prefers_known_agent_descendant() {
        // The fixture is a Windows process tree -- powershell.exe hosting
        // node.exe -- and its path arithmetic is Windows path arithmetic.
        if cfg!(not(windows)) {
            return;
        }

        let helper =
            process_info_for_test(30, 20, "node.exe", vec!["node".to_string()], 30, Vec::new());
        let codex = process_info_for_test(
            40,
            20,
            "node.exe",
            vec![
                "C:\\Users\\me\\AppData\\Roaming\\npm\\codex.cmd".to_string(),
                "--ask-for-approval".to_string(),
            ],
            20,
            Vec::new(),
        );
        let root = process_info_for_test(
            10,
            0,
            "powershell.exe",
            vec!["powershell.exe".to_string()],
            10,
            vec![helper, codex],
        );

        let foreground = foreground_process_summary(&root);
        assert_eq!(foreground.pid, 40);
        assert_eq!(foreground.detected_agent.as_deref(), Some("codex"));
        assert_eq!(foreground.cwd.as_deref(), Some("C:\\work\\node.exe-40"));
        let snapshot = snapshot_from_root(&root);
        assert_eq!(
            snapshot.root_cwd.as_deref(),
            Some("C:\\work\\powershell.exe-10")
        );
        assert_eq!(
            snapshot.foreground_cwd.as_deref(),
            Some("C:\\work\\node.exe-40")
        );
        assert_eq!(
            detect_agent_in_process_tree(&root).as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn summary_uses_newest_child_without_agent_match() {
        let older = process_info_for_test(30, 10, "git.exe", Vec::new(), 30, Vec::new());
        let newer = process_info_for_test(40, 10, "cargo.exe", Vec::new(), 40, Vec::new());
        let root = process_info_for_test(10, 0, "cmd.exe", Vec::new(), 10, vec![older, newer]);

        let foreground = foreground_process_summary(&root);
        assert_eq!(foreground.pid, 40);
        assert_eq!(foreground.name, "cargo.exe");
        assert_eq!(foreground.detected_agent, None);
    }

    /// The walks in this file -- `count_descendants`,
    /// `foreground_process_summary`, `detect_agent_in_process_tree` -- are all
    /// unguarded recursion, and that is fine *because* `LocalProcessInfo` is a
    /// strict tree: `children` owns its children, so a cycle is not even
    /// representable here. The hazard lives one layer down, in the flat
    /// pid/ppid snapshot that `procinfo` turns into that tree; a recycled pid
    /// can make a process claim a descendant of its own as its parent. This
    /// test drives the pathological snapshot through the real builder and
    /// checks that what comes out the other side still adds up: three
    /// processes, two of them descendants of the root, counted once each.
    #[test]
    fn a_cyclic_snapshot_still_produces_a_self_consistent_count() {
        struct Entry {
            pid: u32,
            ppid: u32,
        }

        // 10 -> 20 -> 30, and 30 claims 10 as its parent.
        let procs = vec![
            Entry { pid: 10, ppid: 30 },
            Entry { pid: 20, ppid: 10 },
            Entry { pid: 30, ppid: 20 },
        ];
        let root = LocalProcessInfo::build_tree(
            &procs[0],
            &procs,
            |entry| entry.pid,
            |entry| entry.ppid,
            |entry| {
                process_info_for_test(
                    entry.pid,
                    entry.ppid,
                    "zsh",
                    Vec::new(),
                    entry.pid as u64,
                    Vec::new(),
                )
            },
        );

        assert_eq!(count_descendants(&root), 2);
        // The newest process wins the foreground pick, and the root must not
        // have been re-materialised somewhere below itself to win it.
        assert_eq!(foreground_process_summary(&root).pid, 30);
        assert_eq!(detect_agent_in_process_tree(&root), None);
    }

    #[test]
    fn fallback_reports_known_root_agent_without_process_tree() {
        let snapshot = snapshot(None, "codex").expect("fallback snapshot");
        assert_eq!(snapshot.foreground_process, "codex");
        assert_eq!(snapshot.detected_agent.as_deref(), Some("codex"));
        assert_eq!(snapshot.child_count, 0);
    }

    #[test]
    fn fallback_reports_antigravity_agent() {
        let snapshot = snapshot(None, "agy.exe").expect("fallback snapshot");
        assert_eq!(snapshot.foreground_process, "agy.exe");
        assert_eq!(snapshot.detected_agent.as_deref(), Some("antigravity"));
    }
}
