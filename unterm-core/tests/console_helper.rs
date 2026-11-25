//! The Core's console helper: a copy of the Core that reaches another
//! console (Ctrl+C in a terminal) so the Core keeps its own hidden one.

use std::process::{Command, Stdio};

fn helper(args: &[&str]) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_unterm-core"))
        .arg("--console-ctrl-helper")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run the console helper")
        .code()
}

#[test]
fn a_helper_without_a_target_declines() {
    assert_eq!(helper(&[]), Some(2));
    assert_eq!(helper(&["interrupt"]), Some(2));
    assert_eq!(helper(&["interrupt", "0"]), Some(2));
    assert_eq!(helper(&["interrupt", "4294967295"]), Some(2));
    assert_eq!(helper(&["unknown", "1234"]), Some(2));
}

#[cfg(windows)]
#[test]
fn the_helper_reaches_another_console_and_the_caller_keeps_its_own() {
    use std::os::windows::process::CommandExt;
    use std::time::Duration;

    // A console of the child's own, hidden: the helper attaches to it as it
    // would to a terminal's.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new("cmd.exe")
        .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("start a long command");
    std::thread::sleep(Duration::from_millis(500));

    let pid = child.id().to_string();
    assert_eq!(
        helper(&["mode", &pid]),
        Some(0),
        "cmd expects Ctrl+C to be a signal"
    );
    assert_eq!(helper(&["interrupt", &pid]), Some(0), "Ctrl+C was raised");

    // Whether cmd then stops is up to cmd: a test harness may have it
    // ignore Ctrl+C, which is why `stop_foreground` follows the event with
    // `TerminateProcess`. What matters here is that the helper reached the
    // console and this process -- the Core's stand-in -- is still here.
    let _ = child.kill();
    let _ = child.wait();
}
