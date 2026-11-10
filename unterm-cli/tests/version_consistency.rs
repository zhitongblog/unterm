use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn cli_version_uses_the_shared_product_version_and_exits_quickly() {
    // Warm up macOS syspolicyd / Gatekeeper first-exec scan on the freshly
    // uplifted binary before measuring warm CLI startup latency.
    let _ = Command::new(env!("CARGO_BIN_EXE_unterm-cli"))
        .arg("--version")
        .output();

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_unterm-cli"))
        .arg("--version")
        .output()
        .expect("run unterm-cli --version");

    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("unterm-cli {}", unterm_protocol::PRODUCT_VERSION)
    );
}
