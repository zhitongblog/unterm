//! Updating from inside the window: the notice that a release exists, and
//! the palette's "Install update".
//!
//! The work is `unterm_services::updater`, shared with `unterm-cli update`
//! and Web Settings; this is only where the window hears about it. The
//! install runs on its own thread -- a download takes a while and the window
//! must keep drawing -- and hands what it has to say back through a queue the
//! housekeeping pass drains into notices.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use unterm_services::i18n::t_args;
use unterm_services::updater::Outcome;

static NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALLING: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: Mutex<Option<String>> = Mutex::new(None);

fn say(text: String) {
    if let Ok(mut notices) = NOTICES.lock() {
        notices.push(text);
    }
    crate::mcp_host::wake_loop();
}

/// The newer release the background check found, if it found one.
pub fn available() -> Option<String> {
    let state = unterm_settings::update_check::read_state();
    if !state.get("upgrade_available").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let tag = state.get("latest_tag")?.as_str()?.to_string();
    unterm_services::updater::is_newer(&tag, unterm_protocol::PRODUCT_VERSION).then_some(tag)
}

/// What the window should say now: the install's progress and outcome, and
/// -- once per release -- that a newer one exists.
pub fn take_notices() -> Vec<String> {
    let mut out: Vec<String> = NOTICES
        .lock()
        .map(|mut notices| std::mem::take(&mut *notices))
        .unwrap_or_default();
    if let Some(tag) = available() {
        if let Ok(mut announced) = ANNOUNCED.lock() {
            if announced.as_deref() != Some(tag.as_str()) {
                *announced = Some(tag.clone());
                out.push(t_args("update.available", &[("tag", &tag)]));
            }
        }
    }
    out
}

/// Download, verify and schedule the update, then quit so it can be put in
/// place. Unterm comes back on its own when the helper is done.
pub fn install() {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        say(t_args("update.in_progress", &[]));
        return;
    }
    let tag = available().unwrap_or_default();
    say(t_args("update.downloading", &[("tag", &tag)]));
    let spawned = std::thread::Builder::new()
        .name("unterm-update".into())
        .spawn(move || {
            let result = unterm_services::updater::update_this(true, |_, _| {});
            INSTALLING.store(false, Ordering::SeqCst);
            match result {
                Ok(Outcome::Scheduled { latest, .. }) => {
                    log::info!("update to {latest} scheduled; quitting");
                    crate::engine_backend::request_external_quit();
                }
                Ok(Outcome::UpToDate { current, .. }) => {
                    say(t_args("update.up_to_date", &[("version", &current)]));
                }
                Ok(Outcome::Manual { latest, reason, url, .. }) => {
                    say(t_args(
                        "update.manual",
                        &[("tag", &latest), ("reason", &reason), ("url", &url)],
                    ));
                }
                Err(err) => {
                    log::warn!("update failed: {err:#}");
                    say(t_args("update.failed", &[("err", &format!("{err:#}"))]));
                }
            }
        });
    if spawned.is_err() {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}
