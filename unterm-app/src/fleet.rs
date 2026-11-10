//! Launch a crew of agents on one task.
//!
//! Type the task, pick a crew, press Enter. Each agent gets its own git
//! worktree and its own tab, so they cannot tread on each other, and what they
//! produce is reviewed side by side afterwards.
//!
//! One card and no sub-menus, which is the shape the previous front end
//! settled on: the line you type into is the task, and the rows are crews.
//! Asking for the task and the crew in two steps means the crew is chosen
//! before the task is written, and the task is what tells you how many agents
//! it is worth.
//!
//! The crews come from the agents actually on PATH. Offering `claude + codex`
//! on a machine with no codex is offering a launch that fails afterwards, when
//! the worktrees already exist.

/// A crew: what it is called, and who is in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crew {
    pub label: String,
    pub agents: Vec<String>,
}

/// The crews worth offering, given who is installed.
///
/// Ported unchanged from the previous front end, including the order. The
/// combinations come first because they are the reason the feature exists --
/// two models on one task disagree in useful ways -- and the singles come
/// last, one per installed agent, so the list is never empty on a machine
/// with only one.
pub fn crews(installed: &[&str]) -> Vec<Crew> {
    let has = |name: &str| installed.contains(&name);
    let crew = |label: &str, agents: Vec<&str>| Crew {
        label: label.to_string(),
        agents: agents.into_iter().map(str::to_string).collect(),
    };

    let mut crews = Vec::new();
    if has("claude") {
        crews.push(crew("claude \u{00D7}2", vec!["claude", "claude"]));
        crews.push(crew("claude \u{00D7}3", vec!["claude", "claude", "claude"]));
    }
    if has("agy") {
        crews.push(crew("agy \u{00D7}2", vec!["agy", "agy"]));
    }
    if has("claude") && has("agy") {
        crews.push(crew("claude + agy", vec!["claude", "agy"]));
    }
    if has("claude") && has("codex") {
        crews.push(crew("claude + codex", vec!["claude", "codex"]));
    }
    if has("agy") && has("codex") {
        crews.push(crew("agy + codex", vec!["agy", "codex"]));
    }
    if has("claude") && has("codex") && has("gemini") {
        crews.push(crew(
            "claude + codex + gemini",
            vec!["claude", "codex", "gemini"],
        ));
    }
    for agent in installed {
        crews.push(crew(&format!("{agent} \u{00D7}1"), vec![agent]));
    }
    crews
}

/// Whether a task is worth launching a crew for.
///
/// Blank is not. Every agent would get an empty prompt, which is a bare
/// newline into whatever each of them happens to be showing -- and by then
/// there are worktrees and tabs to clean up.
pub fn task_is_ready(task: &str) -> bool {
    !task.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(installed: &[&str]) -> Vec<String> {
        crews(installed)
            .into_iter()
            .map(|crew| crew.label)
            .collect()
    }

    /// Nobody installed is nothing to offer -- not an empty crew, which
    /// launches worktrees for no one.
    #[test]
    fn no_agents_installed_is_no_crews() {
        assert!(crews(&[]).is_empty());
    }

    /// One agent still gets a crew, or the feature is unavailable on the most
    /// common machine there is.
    #[test]
    fn one_agent_still_gets_a_crew() {
        assert_eq!(labels(&["aider"]), vec!["aider \u{00D7}1"]);
    }

    /// The combinations come first: two models disagreeing usefully is the
    /// reason the feature exists, and a list that opens on `claude ×1` reads
    /// as a launcher rather than as a crew picker.
    #[test]
    fn the_combinations_are_offered_before_the_singles() {
        let labels = labels(&["claude", "codex", "gemini"]);
        let first_single = labels
            .iter()
            .position(|label| label.ends_with("\u{00D7}1"))
            .expect("there are singles");
        assert!(
            labels[..first_single]
                .iter()
                .any(|label| label.contains('+')),
            "{labels:?}"
        );
    }

    /// Never a crew containing somebody who is not installed. Offering it is
    /// offering a launch that fails once the worktrees already exist.
    #[test]
    fn a_crew_never_contains_an_agent_that_is_not_there() {
        for installed in [
            vec![],
            vec!["claude"],
            vec!["codex"],
            vec!["claude", "gemini"],
            vec!["claude", "codex"],
            vec!["claude", "codex", "gemini", "aider"],
        ] {
            for crew in crews(&installed) {
                for agent in &crew.agents {
                    assert!(
                        installed.contains(&agent.as_str()),
                        "{:?} offers {agent}, which is not installed",
                        crew.label
                    );
                }
            }
        }
    }

    /// A crew of nobody is not a crew.
    #[test]
    fn every_crew_has_somebody_in_it() {
        for crew in crews(&["claude", "codex", "gemini", "aider"]) {
            assert!(!crew.agents.is_empty(), "{:?} is empty", crew.label);
        }
    }

    /// The label says who is in it and how many, so a row can be read without
    /// opening anything.
    #[test]
    fn a_label_says_what_the_crew_is() {
        let crews = crews(&["claude", "codex"]);
        let pair = crews
            .iter()
            .find(|crew| crew.agents.len() == 2 && crew.agents[0] != crew.agents[1])
            .expect("a mixed pair");
        assert_eq!(pair.label, "claude + codex");
        let double = crews
            .iter()
            .find(|crew| crew.agents == vec!["claude", "claude"])
            .expect("a doubled crew");
        assert_eq!(double.label, "claude \u{00D7}2");
    }

    /// Blank is not a task. Every agent would get a bare newline into whatever
    /// it happened to be showing, and by then there are worktrees to clean up.
    #[test]
    fn a_blank_task_is_not_ready() {
        assert!(!task_is_ready(""));
        assert!(!task_is_ready("   \t "));
        assert!(task_is_ready("fix the flaky test"));
    }
}
