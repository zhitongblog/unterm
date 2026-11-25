//! Making the front door the only door.
//!
//! Everything M5 built — leases, evidence, revocation, the audit trail — is
//! worth exactly as much as the difficulty of going around it. A model that
//! can run `curl http://127.0.0.1:9222/json` or `npx playwright` has a
//! browser with no lease, no record and nothing to revoke, and every panel
//! Unterm shows about what it did is then a polite fiction.
//!
//! So a managed brain's shell is checked for the ways round: raw CDP, the
//! automation libraries, the drivers, and launching a browser with a
//! debugging port of its own. Refused with the thing to do instead, because
//! a refusal that does not say "use provider.call" is one the model will work
//! around rather than comply with.
//!
//! **This is not a sandbox.** A determined model can obfuscate a command
//! beyond what pattern-matching sees, and nothing here stops it — the same
//! honest limit as the approval boundary. What it does do is make the
//! supported path the easy one and the way round a deliberate act that
//! leaves a refusal in the trail. Enforcement that cannot be evaded needs the
//! operating system, and that is a different piece of work than this.

use serde_json::Value;

/// Why a command was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detour {
    /// What was recognised: `cdp`, `automation_library`, `webdriver`,
    /// `browser_launch`.
    pub kind: &'static str,
    /// The fragment that matched, so a person reading the refusal can see
    /// what tripped it rather than guessing.
    pub matched: String,
    /// What to do instead.
    pub instead: &'static str,
}

impl std::fmt::Display for Detour {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this drives a browser directly ({}: {:?}). {}",
            self.kind, self.matched, self.instead
        )
    }
}

const USE_THE_PROVIDER: &str =
    "Use provider.acquire for the browser capability and provider.call, so the work is leased, \
     recorded and revocable.";

/// Whether a shell command is a way around the provider.
///
/// Matched on the command text with word-ish boundaries rather than bare
/// `contains`: a repository called `playwright-notes` is not an attempt to
/// drive a browser, and a check that cannot tell them apart gets turned off.
pub fn detour_in_command(command: &str) -> Option<Detour> {
    let lowered = command.to_lowercase();

    // The flags that open the protocol, whoever passes them.
    for needle in ["--remote-debugging-port", "--remote-debugging-pipe"] {
        if lowered.contains(needle) {
            return Some(Detour {
                kind: "cdp",
                matched: needle.to_string(),
                instead: USE_THE_PROVIDER,
            });
        }
    }

    // Talking to one. Both halves are required — a loopback address with a
    // port, *and* a devtools path — because either alone is ordinary work:
    // `curl localhost:8080` is somebody's dev server, and
    // `example.com/json/version-history` is a web page.
    if let Some(matched) = local_devtools_endpoint(&lowered) {
        return Some(Detour {
            kind: "cdp",
            matched,
            instead: USE_THE_PROVIDER,
        });
    }

    for (token, kind) in [
        ("playwright", "automation_library"),
        ("puppeteer", "automation_library"),
        ("selenium", "webdriver"),
        ("webdriver", "webdriver"),
        ("chromedriver", "webdriver"),
        ("geckodriver", "webdriver"),
        ("undetected_chromedriver", "webdriver"),
    ] {
        if contains_token(&lowered, token) {
            return Some(Detour {
                kind,
                matched: token.to_string(),
                instead: USE_THE_PROVIDER,
            });
        }
    }

    // Starting a browser at all. Headless or not: a second browser is a
    // second identity, with the user's profile or without it.
    const BROWSERS: &[&str] = &[
        "chromium",
        "google-chrome",
        "chrome.exe",
        "msedge",
        "firefox",
        "brave-browser",
    ];
    if BROWSERS.iter().any(|browser| contains_token(&lowered, browser)) {
        let starts_it = lowered.contains("--headless")
            || lowered.contains("--user-data-dir")
            || lowered.contains("--remote-debugging");
        if starts_it {
            return Some(Detour {
                kind: "browser_launch",
                matched: "a browser with automation flags".to_string(),
                instead: USE_THE_PROVIDER,
            });
        }
    }
    None
}

/// A loopback address with a port, next to a devtools path.
///
/// Returned as the matched fragment so a refusal can show what tripped it.
fn local_devtools_endpoint(lowered: &str) -> Option<String> {
    const LOOPBACK: &[&str] = &["127.0.0.1:", "localhost:", "[::1]:", "0.0.0.0:"];
    const PATHS: &[&str] = &["/json", "/devtools"];
    let host = LOOPBACK.iter().find(|host| lowered.contains(**host))?;
    if !PATHS.iter().any(|path| lowered.contains(path)) {
        return None;
    }
    // Report the whole endpoint rather than just the host, so the message
    // names the thing rather than a prefix of it.
    let start = lowered.find(host)?;
    let fragment: String = lowered[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '\'' && *c != '"' && *c != '|')
        .collect();
    Some(fragment)
}

/// Whether `token` appears in `text` as its own word-ish run.
///
/// Package names carry hyphens and dots, so those count as part of a token;
/// letters and digits either side mean it is part of a longer name.
fn contains_token(text: &str, token: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = text[from..].find(token) {
        let start = from + offset;
        let end = start + token.len();
        let before_ok = start == 0
            || !text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = end == text.len()
            || !text[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Check one tool request from a brain.
///
/// Only shell-ish tools carry commands; everything else passes through here
/// untouched and is judged by the gateway as usual. The tool's *name* is
/// deliberately not consulted — every CLI spells its shell tool differently,
/// and what matters is what the command would do.
pub fn detour_in_tool(_name: &str, arguments: &Value) -> Option<Detour> {
    let command = arguments
        .get("command")
        .or_else(|| arguments.get("cmd"))
        .or_else(|| arguments.get("script"))
        .and_then(Value::as_str);
    match command {
        Some(command) => detour_in_command(command),
        // A tool that names a URL can still be a way round: fetching the CDP
        // endpoint over HTTP is the same detour with a different spelling.
        None => arguments
            .get("url")
            .and_then(Value::as_str)
            .and_then(detour_in_command),
    }
}

/// The gateway method that stands for "this workspace may use its own
/// automation stack".
///
/// A grant on this is how an exception is given: it inherits the whole grant
/// machinery — a TTL, an actor, a task, revocation — rather than growing a
/// second permission system with its own bugs. There is no way to turn it on
/// globally, which is the point: an exception without a scope is a setting.
pub const EXCEPTION_METHOD: &str = "brain.automation_exception";

/// Whether somebody has been allowed to go around the provider, and said so.
///
/// Checked *after* a detour is recognised, so the audit trail records what
/// would have been refused and on whose authority it was not. An exception
/// that leaves no trace is indistinguishable from a hole.
pub fn exception_for(actor: Option<&str>, task_id: Option<&str>) -> Option<String> {
    let mut context = unterm_gateway::ActionContext::new(EXCEPTION_METHOD);
    context.actor = actor.map(str::to_string);
    context.task_id = task_id.map(str::to_string);
    // Asked at the risk the exception actually covers: going around the
    // provider drives a browser directly, and that reaches off this machine.
    // It used to ask at `Destructive`, which was never true of it -- with
    // only three bands, the top one was the only way to say "this needs a
    // serious grant, not a passing one". Now that there is a band for it,
    // saying so plainly is also what lets the exception keep working: the
    // top three are single-use by contract, and an escape hatch that expires
    // on its own is exactly the standing, time-boxed thing they forbid.
    crate::gateway::grant_covering(&context, unterm_gateway::Risk::ExternalSideEffect)
}

/// What the browser capability must never fall back to.
///
/// M6's third gate, in one sentence: when the provider is offline the work
/// waits. It does not quietly reach for another browser stack, because a
/// fallback is a second identity, a second cookie jar and a second set of
/// fingerprints — the exact thing the user chose this browser to avoid.
pub fn fallback_is_never_allowed() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_ways_round_are_refused() {
        for command in [
            "curl http://127.0.0.1:9222/json/version",
            "curl -s localhost:9222/json/list | jq",
            "chromium --headless --remote-debugging-port=9222",
            "npx playwright test",
            "pip install undetected_chromedriver",
            "python -m selenium.webdriver",
            "node -e \"require('puppeteer').launch()\"",
            "chromedriver --port=4444",
            "google-chrome --user-data-dir=/tmp/x --headless",
        ] {
            let detour = detour_in_command(command)
                .unwrap_or_else(|| panic!("{command} was not recognised as a detour"));
            assert!(
                detour.instead.contains("provider.call"),
                "the refusal does not say what to do instead: {detour}"
            );
        }
    }

    #[test]
    fn ordinary_work_is_not_refused() {
        // A check that fires on innocent commands is a check somebody turns
        // off, and then none of it is enforced.
        for command in [
            "cargo test -p unterm-providers",
            "git commit -m 'notes on playwrights and their plays'",
            "grep -r playwright_notes ./docs",
            "ls ~/projects/chromium-notes",
            "echo 'the webdriver_history.md file'",
            "curl https://example.com/json/version-history",
            "firefox",
            "open -a 'Unzoo Browser'",
        ] {
            assert_eq!(
                detour_in_command(command),
                None,
                "{command} was wrongly refused"
            );
        }
    }

    #[test]
    fn a_url_argument_is_checked_too() {
        // The same detour, spelled as a fetch rather than a shell command.
        let detour = detour_in_tool(
            "WebFetch",
            &json!({"url": "http://127.0.0.1:9222/json/version"}),
        );
        assert_eq!(detour.map(|d| d.kind), Some("cdp"));
    }

    #[test]
    fn a_tool_with_no_command_is_none_of_this_modules_business() {
        assert_eq!(detour_in_tool("Read", &json!({"file_path": "/tmp/x"})), None);
        assert_eq!(detour_in_tool("Bash", &json!({})), None);
    }

    #[test]
    fn a_package_name_that_merely_contains_the_word_is_left_alone() {
        assert_eq!(detour_in_command("cat playwrightsnotes.txt"), None);
        assert_eq!(detour_in_command("cd my_selenium_diary"), None);
        // But the real thing, wherever it sits in the line, is caught.
        assert!(detour_in_command("cd /tmp && npx playwright codegen").is_some());
    }

    #[test]
    fn an_exception_is_a_grant_with_a_clock_on_it() {
        // The compatibility escape hatch for a development workspace: given
        // by the layer above, checked here, expiring on its own. Not a
        // setting — an exception without a scope is a setting, and settings
        // are never turned back off.
        let _dir = crate::test_env::IsolatedEnv::new();

        assert_eq!(exception_for(Some("codex"), Some("tsk_1")), None);

        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let grant = store
            .create_grant(unterm_tasks::NewGrant {
                scope_or_once: Some(unterm_tasks::Scope::Task),
                method: Some(EXCEPTION_METHOD.to_string()),
                actor: Some("codex".into()),
                task_id: Some("tsk_1".into()),
                resource: None,
                max_risk: Some("destructive".into()),
                ttl_seconds: Some(300),
            })
            .unwrap();
        assert_eq!(
            exception_for(Some("codex"), Some("tsk_1")).as_deref(),
            Some(grant.id.as_str())
        );
        // Scoped to that task: another one is still fenced in.
        assert_eq!(exception_for(Some("codex"), Some("tsk_2")), None);

        // And revoking it closes the hatch immediately.
        store.revoke_grant(&grant.id).unwrap();
        assert_eq!(exception_for(Some("codex"), Some("tsk_1")), None);
    }

    #[test]
    fn an_offline_browser_makes_work_wait_and_closes_the_other_stacks() {
        // M6's third gate, composed: when the provider is not there the work
        // waits, and every route that would quietly substitute a different
        // browser is refused. A fallback would be a second identity, a second
        // cookie jar and a second set of fingerprints — the exact thing the
        // user chose this browser to avoid.
        use std::sync::Arc;
        use unterm_providers::fake::{registered, FakeProvider};
        use unterm_providers::registry::{Acquire, Registry, WAITING_PROVIDER};
        use unterm_providers::Capability;
        use unterm_tasks::{NewLease, TaskStore};

        let _dir = crate::test_env::IsolatedEnv::new();

        let store = Arc::new(TaskStore::in_memory().unwrap());
        let registry = Registry::new(Arc::clone(&store));
        let browser = Arc::new(FakeProvider::new("browser"));
        registered(&registry, Arc::clone(&browser));
        registry.bind("browser").unwrap();

        browser.go_offline();
        let _ = registry.bind("browser");
        let waiting = registry
            .acquire(Capability::Browser, NewLease::default())
            .unwrap();
        match waiting {
            Acquire::Waiting { reason, .. } => assert_eq!(reason, WAITING_PROVIDER),
            other => panic!("an offline browser did not make the work wait: {other:?}"),
        }

        for fallback in [
            "npx playwright open https://example.com",
            "chromium --headless --user-data-dir=/tmp/fallback https://example.com",
            "python -c \"from selenium import webdriver\"",
            "curl http://127.0.0.1:9222/json/new?https://example.com",
        ] {
            assert!(
                detour_in_command(fallback).is_some(),
                "{fallback} would have substituted another browser stack"
            );
        }
        assert!(fallback_is_never_allowed());
    }

    #[test]
    fn a_browser_started_without_automation_flags_is_not_a_detour() {
        // Opening a browser to look at something is not driving one, and
        // refusing it would make Unterm unusable for ordinary work.
        assert_eq!(detour_in_command("firefox https://example.com"), None);
        assert!(detour_in_command("firefox --headless --user-data-dir=/tmp/p").is_some());
    }
}
