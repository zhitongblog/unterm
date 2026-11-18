//! Other AI clients on this machine, and whether they can use this terminal.
//!
//! Claude Desktop, Claude Code, Cursor, Codex and the rest each keep a list
//! of MCP servers in a config file of their own. Putting this terminal in
//! that list (`unterm-cli mcp-stdio`, under the product's name) lets an agent
//! the person started there drive the terminal here -- with the same
//! gateway, the same audit, and its questions answered in the console
//! (`note_console_watching` in unterm-mcp).
//!
//! This reads and edits only our own entry, keeps everything else in the
//! file as it was (in content; JSON is rewritten pretty-printed), and writes
//! through a temporary file so a crash never leaves half a config. It also
//! reports whether each client already has Unzoo Browser's server (`unzoo`),
//! which Unzoo Browser's own installer manages -- this module never writes
//! that one.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    /// `mcpServers: {name: {type: "stdio", command, args}}`.
    Servers,
    /// The same without `type`: Claude Desktop's shape.
    DesktopServers,
    /// `mcp: {name: {type: "local", enabled, command: [...]}}`: OpenCode.
    OpenCode,
    /// `[mcp_servers.name]` with `command` and `args`: Codex.
    Toml,
}

struct Client {
    id: &'static str,
    label: &'static str,
    /// Home-relative; the client is here if any exists.
    detect: Vec<&'static str>,
    /// Home-relative config file.
    config: &'static str,
    format: Format,
    /// Home-relative folder the client reads Agent Skills from, when it has
    /// one: connecting puts Unzoo One's skills there too.
    skills: Option<&'static str>,
}

fn desktop_config() -> &'static str {
    if cfg!(windows) {
        "AppData/Roaming/Claude/claude_desktop_config.json"
    } else if cfg!(target_os = "macos") {
        "Library/Application Support/Claude/claude_desktop_config.json"
    } else {
        ".config/Claude/claude_desktop_config.json"
    }
}

fn desktop_dir() -> &'static str {
    if cfg!(windows) {
        "AppData/Roaming/Claude"
    } else if cfg!(target_os = "macos") {
        "Library/Application Support/Claude"
    } else {
        ".config/Claude"
    }
}

fn table() -> Vec<Client> {
    vec![
        Client { id: "claude-desktop", label: "Claude Desktop", detect: vec![desktop_dir()], config: desktop_config(), format: Format::DesktopServers, skills: None },
        Client { id: "claude-code", label: "Claude Code", detect: vec![".claude.json", ".claude"], config: ".claude.json", format: Format::Servers, skills: Some(".claude/skills") },
        Client { id: "cursor", label: "Cursor", detect: vec![".cursor"], config: ".cursor/mcp.json", format: Format::Servers, skills: None },
        Client { id: "codex", label: "Codex", detect: vec![".codex"], config: ".codex/config.toml", format: Format::Toml, skills: Some(".codex/skills") },
        Client { id: "windsurf", label: "Windsurf", detect: vec![".codeium/windsurf"], config: ".codeium/windsurf/mcp_config.json", format: Format::Servers, skills: None },
        Client { id: "agy", label: "Antigravity", detect: vec![".gemini/antigravity-cli", ".gemini/config"], config: ".gemini/config/mcp_config.json", format: Format::Servers, skills: None },
        Client { id: "gemini", label: "Gemini CLI", detect: vec![".gemini"], config: ".gemini/settings.json", format: Format::Servers, skills: None },
        Client { id: "opencode", label: "OpenCode", detect: vec![".config/opencode"], config: ".config/opencode/opencode.json", format: Format::OpenCode, skills: Some(".config/opencode/skills") },
        Client { id: "kimi", label: "Kimi Code", detect: vec![".kimi"], config: ".kimi/mcp.json", format: Format::Servers, skills: None },
        Client { id: "qwen", label: "Qwen Code", detect: vec![".qwen"], config: ".qwen/settings.json", format: Format::Servers, skills: None },
    ]
}

/// The name our server goes under: the product's, as a slug ("Unzoo One" →
/// `unzoo-one`), so a person reading their client's server list sees the
/// product they installed.
pub fn server_name() -> String {
    unterm_protocol::product_slug()
}

/// Unzoo Browser's own server, which its installer writes.
const BROWSER_SERVER: &str = "unzoo";

fn home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("UNTERM_CLIENTS_HOME").map(PathBuf::from) {
        return Ok(dir);
    }
    dirs_next::home_dir().ok_or_else(|| anyhow!("there is no home folder"))
}

/// The servers a config lists, by name.
fn servers_in(path: &Path, format: Format) -> Result<Vec<(String, Value)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let (value, key) = match format {
        Format::Toml => {
            let parsed: toml::Value = toml::from_str(&text).with_context(|| format!("{} is not valid TOML", path.display()))?;
            (serde_json::to_value(parsed)?, "mcp_servers")
        }
        Format::OpenCode => (parse_json(&text, path)?, "mcp"),
        _ => (parse_json(&text, path)?, "mcpServers"),
    };
    Ok(value
        .get(key)
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default())
}

fn parse_json(text: &str, path: &Path) -> Result<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = serde_json::from_str(text).with_context(|| format!("{} is not valid JSON", path.display()))?;
    if !value.is_object() {
        bail!("{} is not a JSON object", path.display());
    }
    Ok(value)
}

/// Whether an entry runs this terminal's bridge (under any name).
fn runs_bridge(entry: &Value, cli: &str) -> bool {
    let command = entry.get("command");
    let program = match command {
        Some(Value::String(program)) => program.as_str(),
        Some(Value::Array(parts)) => parts.first().and_then(Value::as_str).unwrap_or_default(),
        _ => "",
    };
    let same = |a: &str, b: &str| a.replace('\\', "/").eq_ignore_ascii_case(&b.replace('\\', "/"));
    !program.is_empty() && same(program, cli)
}

/// Every client: whether it is installed here, and whether it has this
/// terminal and Unzoo Browser.
pub fn status(cli: Option<&str>) -> Value {
    let Ok(home) = home() else { return json!([]) };
    let name = server_name();
    json!(table()
        .into_iter()
        .map(|client| {
            let path = home.join(client.config);
            let detected = client.detect.iter().any(|p| home.join(p).exists());
            match servers_in(&path, client.format) {
                Ok(servers) => json!({
                    "id": client.id,
                    "name": client.label,
                    "detected": detected,
                    "config": path.display().to_string(),
                    "terminal": servers.iter().any(|(key, entry)| key == &name || cli.is_some_and(|cli| runs_bridge(entry, cli))),
                    "browser": servers.iter().any(|(key, _)| key == BROWSER_SERVER),
                    "skills": client.skills.is_some(),
                }),
                Err(error) => json!({
                    "id": client.id,
                    "name": client.label,
                    "detected": detected,
                    "config": path.display().to_string(),
                    "terminal": false,
                    "browser": false,
                    "error": format!("{error:#}"),
                }),
            }
        })
        .collect::<Vec<_>>())
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("unzoo-one.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

/// Add this terminal to a client (`connect`), or take it out. Any other entry
/// that runs the same bridge under another name is taken out as well, so the
/// client does not end up with the same tools twice.
pub fn set(id: &str, cli: &str, connect: bool) -> Result<Value> {
    let client = table().into_iter().find(|c| c.id == id).ok_or_else(|| anyhow!("no client named {id}"))?;
    let home = home()?;
    if connect && !client.detect.iter().any(|p| home.join(p).exists()) {
        bail!("{} is not installed for this user", client.label);
    }
    let path = home.join(client.config);
    let name = server_name();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    match client.format {
        Format::Toml => {
            let mut doc: toml::Value = if existing.trim().is_empty() {
                toml::Value::Table(Default::default())
            } else {
                toml::from_str(&existing).with_context(|| format!("{} is not valid TOML", path.display()))?
            };
            let table = doc.as_table_mut().ok_or_else(|| anyhow!("{} is not a TOML table", path.display()))?;
            let servers = table
                .entry("mcp_servers")
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .ok_or_else(|| anyhow!("mcp_servers is not a table"))?;
            servers.retain(|key, entry| {
                key != &name && !serde_json::to_value(entry).map(|e| runs_bridge(&e, cli)).unwrap_or(false)
            });
            if connect {
                let mut entry = toml::value::Table::new();
                entry.insert("command".into(), toml::Value::String(cli.to_string()));
                entry.insert("args".into(), toml::Value::Array(vec![toml::Value::String("mcp-stdio".into())]));
                servers.insert(name.clone(), toml::Value::Table(entry));
            } else if servers.is_empty() {
                table.remove("mcp_servers");
            }
            write_atomically(&path, &toml::to_string_pretty(&doc)?)?;
        }
        format => {
            let mut doc = if existing.trim().is_empty() { json!({}) } else { parse_json(&existing, &path)? };
            let key = if format == Format::OpenCode { "mcp" } else { "mcpServers" };
            let object = doc.as_object_mut().expect("checked above");
            let servers = object
                .entry(key)
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| anyhow!("{key} in {} is not an object", path.display()))?;
            servers.retain(|server, entry| server != &name && !runs_bridge(entry, cli));
            if connect {
                let entry = match format {
                    Format::OpenCode => json!({"type": "local", "enabled": true, "command": [cli, "mcp-stdio"]}),
                    Format::DesktopServers => json!({"command": cli, "args": ["mcp-stdio"]}),
                    _ => json!({"type": "stdio", "command": cli, "args": ["mcp-stdio"]}),
                };
                servers.insert(name.clone(), entry);
            } else if servers.is_empty() {
                object.remove(key);
            }
            write_atomically(&path, &(serde_json::to_string_pretty(&doc)? + "\n"))?;
        }
    }
    let mut reply = json!({"id": id, "name": client.label, "terminal": connect, "config": path.display().to_string()});
    // The skills go with the terminal: they are how an agent learns to use
    // it well. A failure here does not undo the connection; it is reported.
    if let Some(dir) = client.skills.map(|dir| home.join(dir)) {
        let result = if connect {
            super::skills::install_into(&dir).map(|(installed, skipped)| json!({"installed": installed, "skipped": skipped}))
        } else {
            super::skills::remove_from(&dir).map(|removed| json!({"removed": removed}))
        };
        reply["skills"] = result.unwrap_or_else(|error| json!({"error": format!("{error:#}")}));
    }
    Ok(reply)
}

/// Bring the skills up to date in every client that has this terminal and a
/// skills folder: after a skill is switched, imported or deleted, and when
/// the service starts (an update may have changed the built-in ones).
pub fn sync_skills() -> Result<()> {
    let home = home()?;
    let name = server_name();
    for client in table() {
        let Some(dir) = client.skills else { continue };
        let connected = servers_in(&home.join(client.config), client.format)
            .map(|servers| servers.iter().any(|(key, _)| key == &name))
            .unwrap_or(false);
        if connected {
            super::skills::install_into(&home.join(dir))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home<T>(work: impl FnOnce(&Path) -> T) -> T {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_CLIENTS_HOME", home.path());
        std::env::set_var("UNTERM_PRODUCT_NAME", "Unzoo One");
        let out = work(home.path());
        std::env::remove_var("UNTERM_CLIENTS_HOME");
        std::env::remove_var("UNTERM_PRODUCT_NAME");
        out
    }

    fn entry_of<'a>(status: &'a Value, id: &str) -> &'a Value {
        status.as_array().unwrap().iter().find(|c| c["id"] == id).unwrap()
    }

    /// Connecting puts the skills where the client reads them; the person's
    /// own folder of the same name, and skills meant only for hosted runs,
    /// stay out; disconnecting takes back exactly what was put there.
    #[test]
    fn the_skills_go_with_the_terminal() {
        let builtin = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for name in ["unzoo-terminal", "unzoo-deliverables", "unzoo-browser"] {
            std::fs::create_dir_all(builtin.path().join(name)).unwrap();
            std::fs::write(
                builtin.path().join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test skill {name}\n---\n\nbody\n"),
            )
            .unwrap();
        }
        std::env::set_var("UNTERM_BUILTIN_SKILLS", builtin.path());
        std::env::set_var("UNTERM_STATE_DIR", state.path());
        with_home(|home| {
            let cli = "/opt/unzoo-one/unterm-cli";
            std::fs::create_dir_all(home.join(".claude/skills/unzoo-browser")).unwrap();
            std::fs::write(home.join(".claude/skills/unzoo-browser/SKILL.md"), "mine").unwrap();
            std::fs::create_dir_all(home.join(".claude/skills/my-own")).unwrap();

            let reply = set("claude-code", cli, true).unwrap();
            assert_eq!(reply["skills"]["installed"], json!(["unzoo-terminal"]));
            assert_eq!(reply["skills"]["skipped"], json!(["unzoo-browser"]));
            let skills = home.join(".claude/skills");
            assert!(skills.join("unzoo-terminal/SKILL.md").is_file());
            assert!(!skills.join("unzoo-deliverables").exists(), "only meaningful in a hosted run");
            assert_eq!(std::fs::read_to_string(skills.join("unzoo-browser/SKILL.md")).unwrap(), "mine");

            // Switched off here: gone from the connected client too.
            crate::skills::set_enabled("unzoo-terminal", false).unwrap();
            assert!(!skills.join("unzoo-terminal").exists());
            crate::skills::set_enabled("unzoo-terminal", true).unwrap();
            assert!(skills.join("unzoo-terminal/SKILL.md").is_file());

            set("claude-code", cli, false).unwrap();
            assert!(!skills.join("unzoo-terminal").exists());
            assert!(skills.join("unzoo-browser/SKILL.md").is_file(), "the person's own stays");
            assert!(skills.join("my-own").is_dir());

            // A client without a skills folder gets the terminal alone.
            std::fs::create_dir_all(home.join(".cursor")).unwrap();
            assert!(set("cursor", cli, true).unwrap().get("skills").is_none());
        });
        std::env::remove_var("UNTERM_BUILTIN_SKILLS");
        std::env::remove_var("UNTERM_STATE_DIR");
    }

    #[test]
    fn the_server_is_named_after_the_product() {
        std::env::set_var("UNTERM_PRODUCT_NAME", "Unzoo One");
        assert_eq!(server_name(), "unzoo-one");
        std::env::remove_var("UNTERM_PRODUCT_NAME");
        assert_eq!(server_name(), "unterm");
    }

    #[test]
    fn connecting_adds_only_our_entry_and_disconnecting_takes_it_out() {
        with_home(|home| {
            let cli = r"C:\Program Files\Unzoo One\unterm-cli.exe";
            std::fs::write(
                home.join(".claude.json"),
                r#"{"numStartups": 3, "mcpServers": {"unzoo": {"type": "stdio", "command": "unzoo-service.exe"}, "unterm": {"type": "stdio", "command": "C:/Program Files/Unzoo One/unterm-cli.exe", "args": ["mcp-stdio"]}}}"#,
            )
            .unwrap();
            let before = status(Some(cli));
            assert_eq!(entry_of(&before, "claude-code")["terminal"], true, "the old entry runs the same bridge");
            assert_eq!(entry_of(&before, "claude-code")["browser"], true);
            assert_eq!(entry_of(&before, "cursor")["detected"], false);

            set("claude-code", cli, true).unwrap();
            let config: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
            assert_eq!(config["numStartups"], 3, "the rest of the file is kept");
            assert!(config["mcpServers"]["unzoo"].is_object(), "Unzoo Browser's entry is left alone");
            assert!(config["mcpServers"].get("unterm").is_none(), "no second copy of the same tools");
            assert_eq!(config["mcpServers"]["unzoo-one"]["args"], json!(["mcp-stdio"]));

            set("claude-code", cli, false).unwrap();
            let config: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
            assert!(config["mcpServers"].get("unzoo-one").is_none());
            assert!(config["mcpServers"]["unzoo"].is_object());
            assert_eq!(entry_of(&status(Some(cli)), "claude-code")["terminal"], false);

            // A client that is not installed is not written into.
            assert!(set("cursor", cli, true).is_err());
        });
    }

    #[test]
    fn codex_toml_and_claude_desktop_shapes() {
        with_home(|home| {
            let cli = "/opt/unzoo-one/unterm-cli";
            std::fs::create_dir_all(home.join(".codex")).unwrap();
            std::fs::write(home.join(".codex/config.toml"), "model = \"o5\"\n").unwrap();
            set("codex", cli, true).unwrap();
            let text = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
            let parsed: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(parsed["model"].as_str(), Some("o5"));
            assert_eq!(parsed["mcp_servers"]["unzoo-one"]["command"].as_str(), Some(cli));
            set("codex", cli, false).unwrap();
            let parsed: toml::Value = toml::from_str(&std::fs::read_to_string(home.join(".codex/config.toml")).unwrap()).unwrap();
            assert!(parsed.get("mcp_servers").is_none(), "no empty table left behind");

            std::fs::create_dir_all(home.join(desktop_dir())).unwrap();
            set("claude-desktop", cli, true).unwrap();
            let config: Value = serde_json::from_str(&std::fs::read_to_string(home.join(desktop_config())).unwrap()).unwrap();
            assert_eq!(config["mcpServers"]["unzoo-one"], json!({"command": cli, "args": ["mcp-stdio"]}));
        });
    }

    #[test]
    fn a_broken_config_is_reported_not_overwritten() {
        with_home(|home| {
            std::fs::create_dir_all(home.join(".cursor")).unwrap();
            std::fs::write(home.join(".cursor/mcp.json"), "{ not json").unwrap();
            assert!(entry_of(&status(None), "cursor")["error"].is_string());
            assert!(set("cursor", "x", true).is_err());
            assert_eq!(std::fs::read_to_string(home.join(".cursor/mcp.json")).unwrap(), "{ not json");
        });
    }
}
