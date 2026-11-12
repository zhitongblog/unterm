//! Skills the agents run here are given.
//!
//! A skill is the open Agent Skills shape: a folder with a `SKILL.md` whose
//! frontmatter names it and says when to use it, plus whatever files it
//! needs. Two places hold them:
//!
//! - **built in**, shipped beside this binary (`<exe dir>/skills`, or
//!   `UNTERM_BUILTIN_SKILLS`): how to use this machine's terminal and
//!   browser well -- the knowledge that makes an agent use them at all;
//! - **the person's own**, under the state directory: imported, or saved
//!   from a conversation that worked ([`import_saved`]).
//!
//! Each can be switched off. Before an agent starts, the enabled ones are
//! laid out as one plugin ([`prepare_plugin`]) that Claude Code loads for
//! that session only (`--plugin-dir`): nothing is written into the person's
//! own `~/.claude`, and a skill switched off here is gone from the next run.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// The plugin's name; Claude Code shows its skills as `unzoo-one:<skill>`.
const PLUGIN: &str = "unzoo-one";
/// Where an agent writes a skill it is asked to save, inside its folder.
pub const SAVE_DIR: &str = ".unzoo-skill";

fn builtin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("UNTERM_BUILTIN_SKILLS").map(PathBuf::from) {
        return dir.is_dir().then_some(dir);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("skills");
    dir.is_dir().then_some(dir)
}

fn user_dir() -> Result<PathBuf> {
    unterm_protocol::state_path("skills").context("could not resolve the state directory")
}

fn config_path() -> Result<PathBuf> {
    unterm_protocol::state_path("skills.json").context("could not resolve the state directory")
}

fn disabled() -> Vec<String> {
    config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("disabled").cloned())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn write_disabled(names: &[String]) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&json!({"disabled": names}))?)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// A name a skill may have: what Claude Code accepts, and safe as a folder.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// `name` and `description` from a SKILL.md's frontmatter.
pub fn frontmatter(text: &str) -> Option<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.trim_start().strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in rest[..end].lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    Some((name?, description.unwrap_or_default()))
}

fn read_skill(dir: &Path) -> Option<(String, String)> {
    frontmatter(&std::fs::read_to_string(dir.join("SKILL.md")).ok()?)
}

fn skills_in(root: Option<PathBuf>, source: &str) -> Vec<(String, String, String, PathBuf)> {
    let Some(root) = root else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut found: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let (name, description) = read_skill(&entry.path())?;
            valid_name(&name).then(|| (name, description, source.to_string(), entry.path()))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Every skill, built in first. A skill of the person's own with a built-in
/// name is shadowed by the built-in one.
fn all() -> Vec<(String, String, String, PathBuf)> {
    let mut skills = skills_in(builtin_dir(), "builtin");
    for skill in skills_in(user_dir().ok(), "user") {
        if !skills.iter().any(|known| known.0 == skill.0) {
            skills.push(skill);
        }
    }
    skills
}

/// What the console lists.
pub fn list() -> Value {
    let off = disabled();
    json!(all()
        .into_iter()
        .map(|(name, description, source, path)| json!({
            "name": name,
            "description": description,
            "source": source,
            "enabled": !off.contains(&name),
            "path": path.display().to_string(),
        }))
        .collect::<Vec<_>>())
}

pub fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    if !all().iter().any(|skill| skill.0 == name) {
        bail!("no skill named {name}");
    }
    let mut off = disabled();
    off.retain(|known| known != name);
    if !enabled {
        off.push(name.to_string());
    }
    write_disabled(&off)
}

/// Remove a skill of the person's own. Built-in ones can only be switched
/// off: they come back with the next install anyway.
pub fn delete(name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("not a skill name: {name}");
    }
    if skills_in(builtin_dir(), "builtin").iter().any(|skill| skill.0 == name) {
        bail!("{name} is built in; switch it off instead");
    }
    let dir = user_dir()?.join(name);
    if !dir.join("SKILL.md").is_file() {
        bail!("no skill named {name}");
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
    let mut off = disabled();
    off.retain(|known| known != name);
    write_disabled(&off)
}

fn copy_dir(from: &Path, to: &Path, depth: usize) -> Result<()> {
    if depth > 8 {
        bail!("{} is nested too deeply", from.display());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target, depth + 1)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
        // Links are not followed: a skill is its own files.
    }
    Ok(())
}

/// Take in a skill folder (one holding a SKILL.md) as one of the person's
/// own. `replace` overwrites one of theirs with the same name; a built-in
/// name is refused.
pub fn import(from: &Path, replace: bool) -> Result<String> {
    let (name, _) = read_skill(from).ok_or_else(|| anyhow!("{} has no SKILL.md with a name in its frontmatter", from.display()))?;
    if !valid_name(&name) {
        bail!("{name:?} is not a skill name: lowercase letters, digits and dashes, up to 64");
    }
    if skills_in(builtin_dir(), "builtin").iter().any(|skill| skill.0 == name) {
        bail!("{name} is the name of a built-in skill");
    }
    let target = user_dir()?.join(&name);
    if target.exists() {
        if !replace {
            bail!("there is already a skill named {name}");
        }
        std::fs::remove_dir_all(&target)?;
    }
    copy_dir(from, &target, 0)?;
    Ok(name)
}

/// The skills an agent wrote into `<cwd>/.unzoo-skill/` when asked to save
/// what it did: each taken in (replacing an earlier save of the same name),
/// and the folder removed so it is not mistaken for a deliverable.
pub fn import_saved(cwd: &Path) -> Result<Vec<String>> {
    let root = cwd.join(SAVE_DIR);
    let entries = std::fs::read_dir(&root).with_context(|| format!("the agent wrote no skill into {}", root.display()))?;
    let mut saved = Vec::new();
    for entry in entries.flatten() {
        if entry.path().join("SKILL.md").is_file() {
            saved.push(import(&entry.path(), true)?);
        }
    }
    if saved.is_empty() {
        bail!("the agent wrote no SKILL.md under {}", root.display());
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(saved)
}

/// Lay the enabled skills out as one plugin and return its folder, for
/// `--plugin-dir`; `None` when no skill is enabled.
pub fn prepare_plugin() -> Result<Option<PathBuf>> {
    let off = disabled();
    let enabled: Vec<_> = all().into_iter().filter(|skill| !off.contains(&skill.0)).collect();
    if enabled.is_empty() {
        return Ok(None);
    }
    let root = unterm_protocol::state_path("agent-plugin")
        .context("could not resolve the state directory")?
        .join(PLUGIN);
    // Rebuilt every time: a skill switched off or deleted must not linger.
    // A run that is reading the old copy keeps its open files on Unix; on
    // Windows a busy file makes the removal fail, and the old copy is reused.
    if root.exists() && std::fs::remove_dir_all(&root).is_err() {
        return Ok(Some(root));
    }
    std::fs::create_dir_all(root.join(".claude-plugin"))?;
    std::fs::write(
        root.join(".claude-plugin").join("plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "name": PLUGIN,
            "version": "1.0.0",
            "description": "Skills for working on this computer through Unzoo One: its terminal, its browser, and the person's own saved skills.",
        }))?,
    )?;
    for (name, _, _, path) in enabled {
        copy_dir(&path, &root.join("skills").join(name), 0)?;
    }
    Ok(Some(root))
}

/// What an agent is asked when a person saves a conversation as a skill.
pub fn save_prompt() -> String {
    format!(
        "Turn what we did in this conversation into a reusable skill, so it can be done again from one sentence. \
Write it as `{SAVE_DIR}/<short-kebab-case-name>/SKILL.md` in the current folder. Start the file with YAML frontmatter: \
`name` (the same kebab-case name) and `description` (one sentence saying what it does and when to use it). Then write the \
steps that worked, in order: the commands, URLs and page actions, exactly. Write what changes from one use to the next \
as named inputs, and leave out anything that was specific to this one time. Never include passwords, tokens, cookies or \
personal data. If a small script makes the steps reliable, put it next to SKILL.md and say how to run it. Reply with \
the skill's name and one line on what it does; do not mention the folder, Unzoo One files the skill away itself."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_what_claude_code_accepts() {
        assert!(valid_name("unzoo-browser"));
        assert!(valid_name("csv2xlsx"));
        for bad in ["", "Upper", "has space", "-lead", "trail-", "dot.name", "a/b", &"x".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn frontmatter_gives_name_and_description() {
        let text = "\u{feff}---\nname: weekly-report\ndescription: \"Build the weekly report.\"\nlicense: MIT\n---\n\n# Steps\n";
        assert_eq!(frontmatter(text), Some(("weekly-report".into(), "Build the weekly report.".into())));
        assert_eq!(frontmatter("# no frontmatter"), None);
        assert_eq!(frontmatter("---\ndescription: x\n---\n"), None, "a name is required");
    }

    #[test]
    fn skills_are_listed_switched_off_imported_saved_and_laid_out_as_a_plugin() {
        let state = tempfile::tempdir().unwrap();
        let builtin = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", state.path());
        std::env::set_var("UNTERM_BUILTIN_SKILLS", builtin.path());

        let write = |dir: &Path, name: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: about {name}\n---\nbody\n")).unwrap();
        };
        write(&builtin.path().join("unzoo-terminal"), "unzoo-terminal");
        write(&builtin.path().join("not-a-skill-dir").join("deeper"), "ignored");

        // An agent saved what it did.
        let saved = work.path().join(SAVE_DIR).join("weekly-report");
        write(&saved, "weekly-report");
        std::fs::write(saved.join("run.ps1"), "echo hi").unwrap();
        assert_eq!(import_saved(work.path()).unwrap(), vec!["weekly-report".to_string()]);
        assert!(!work.path().join(SAVE_DIR).exists(), "not left behind as a deliverable");

        let names = |list: Value| list.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(list()), vec!["unzoo-terminal", "weekly-report"]);
        assert_eq!(list()[0]["source"], "builtin");

        // A built-in name cannot be taken, a user's own cannot be duplicated.
        let clash = tempfile::tempdir().unwrap();
        write(clash.path(), "unzoo-terminal");
        assert!(import(clash.path(), true).is_err());
        let again = tempfile::tempdir().unwrap();
        write(again.path(), "weekly-report");
        assert!(import(again.path(), false).is_err());

        let plugin = prepare_plugin().unwrap().unwrap();
        assert!(plugin.join(".claude-plugin").join("plugin.json").is_file());
        assert!(plugin.join("skills").join("weekly-report").join("run.ps1").is_file());
        assert!(plugin.join("skills").join("unzoo-terminal").join("SKILL.md").is_file());

        // Switched off: gone from the next plugin, still listed.
        set_enabled("unzoo-terminal", false).unwrap();
        let plugin = prepare_plugin().unwrap().unwrap();
        assert!(!plugin.join("skills").join("unzoo-terminal").exists());
        assert_eq!(list()[0]["enabled"], false);
        assert!(delete("unzoo-terminal").is_err(), "built in: switch off, not delete");

        delete("weekly-report").unwrap();
        assert!(prepare_plugin().unwrap().is_none(), "nothing enabled, nothing to load");

        std::env::remove_var("UNTERM_BUILTIN_SKILLS");
        std::env::remove_var("UNTERM_STATE_DIR");
    }
}
