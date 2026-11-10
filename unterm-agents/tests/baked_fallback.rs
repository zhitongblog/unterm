//! End-to-end test: the baked-in fallback envelope passes signature
//! verification with the baked trusted keys. This catches the worst-case
//! regression: someone updates `keys/trusted.json` without re-signing the
//! envelope (or vice versa) and ships a binary that fails to verify its
//! own bundled fallback.

use unterm_agents::{envelope::verify_envelope, fetch_manifests_offline};

#[test]
fn baked_envelope_verifies_and_lists_known_agents() {
    // First delete any stale on-disk cache from previous test runs in this
    // home dir — we want to force the baked path.
    if let Ok(cache) = unterm_agents::paths::manifest_cache_path() {
        let _ = std::fs::remove_file(cache);
    }

    let baked: &[u8] = include_bytes!("../baked/manifests-fallback.json");
    let envelope = verify_envelope(baked).expect("baked envelope must verify");

    // Catches the "signed with old key" regression where keys/trusted.json
    // and baked/manifests-fallback.json drift apart.
    assert!(
        envelope.manifests.len() >= 1,
        "baked envelope should ship with at least one agent manifest"
    );

    // The known agents we author by hand. If a release drops one,
    // update both the manifest folder and this test.
    let ids: Vec<&str> = envelope.manifests.iter().map(|m| m.id.as_str()).collect();
    let must = [
        "agy",
        "claude-code",
        "codex-cli",
        "gemini-cli",
        "opencode",
        "aider",
        "kimi-code",
        "qwen-code",
        "trae-agent",
    ];
    for id in must {
        assert!(
            ids.contains(&id),
            "baked envelope missing first-party manifest {id:?}; have {ids:?}"
        );
    }

    let kimi = envelope
        .manifests
        .iter()
        .find(|m| m.id == "kimi-code")
        .expect("kimi-code manifest present");
    let kimi_install_text =
        serde_json::to_string(&kimi.install).expect("serialize kimi install spec");
    assert!(
        !kimi_install_text.contains("@moonshot-ai/kimi-code"),
        "Kimi Code should use the official installer, not the legacy npm package"
    );
    assert!(
        kimi_install_text.contains("https://code.kimi.com/kimi-code/install.sh")
            && kimi_install_text.contains("https://code.kimi.com/kimi-code/install.ps1"),
        "Kimi Code manifest should advertise official macOS/Linux and Windows installers"
    );

    let agy = envelope
        .manifests
        .iter()
        .find(|m| m.id == "agy")
        .expect("agy manifest present");
    let agy_install_text =
        serde_json::to_string(&agy.install).expect("serialize agy install spec");
    assert!(
        agy_install_text.contains("https://antigravity.google/cli/install.sh")
            && agy_install_text.contains("https://antigravity.google/cli/install.ps1"),
        "Antigravity CLI (agy) manifest should advertise official macOS/Linux and Windows installers"
    );
}

#[test]
fn baked_envelope_advertises_a_recent_min_version() {
    let set = fetch_manifests_offline().unwrap();
    // The baked fallback's min_unterm_version should at least be older or
    // equal to the running crate version — if it's newer, every install
    // refuses to use the fallback.
    let pkg = env!("CARGO_PKG_VERSION");
    assert!(
        unterm_agents::envelope::compare_versions(pkg, &set.envelope.min_unterm_version) >= 0,
        "baked envelope requires Unterm >= {} but we are {pkg}",
        set.envelope.min_unterm_version
    );
}
