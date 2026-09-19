param(
    [switch]$ListOnly
)

$ErrorActionPreference = "Stop"

$CiDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $CiDir "..")

$TestFilter = "handler::engine_neutral_handler_tests::"
$RequiredTests = @(
    "handler::engine_neutral_handler_tests::server_health_uses_selected_next_core_engine",
    "handler::engine_neutral_handler_tests::server_health_exposes_next_core_io_summary",
    "handler::engine_neutral_handler_tests::capability_surfaces_expose_next_core_health_io_diagnostics",
    "handler::engine_neutral_handler_tests::selftest_run_uses_selected_terminal_engine",
    "handler::engine_neutral_handler_tests::screen_search_goto_scrolls_next_core_logical_viewport",
    "handler::engine_neutral_handler_tests::screen_scroll_goto_updates_next_core_logical_viewport",
    "handler::engine_neutral_handler_tests::screen_scrollback_text_resolves_active_next_core_session_without_pane_param",
    "handler::engine_neutral_handler_tests::screen_clear_drops_history_and_keeps_the_screen",
    "handler::engine_neutral_handler_tests::session_destroy_uses_next_core_pane_id_path",
    "handler::engine_neutral_handler_tests::session_resize_uses_next_core_pane_id_path",
    "handler::engine_neutral_handler_tests::session_env_reads_next_core_launch_env_keys_without_values",
    "handler::engine_neutral_handler_tests::session_set_env_applies_next_core_future_launch_overlay_without_values",
    "handler::engine_neutral_handler_tests::activity_methods_expose_next_core_io_metrics",
    "handler::engine_neutral_handler_tests::core_status_history_cursor_methods_use_next_core_engine",
    "handler::engine_neutral_handler_tests::screen_detect_errors_uses_next_core_screen_snapshot",
    "handler::engine_neutral_handler_tests::capture_screen_text_snapshot_uses_terminal_engine",
    "handler::engine_neutral_handler_tests::capture_window_text_snapshot_uses_terminal_engine",
    "handler::engine_neutral_handler_tests::capture_scrollback_without_a_host_reports_no_front_end",
    "handler::engine_neutral_handler_tests::orchestrate_methods_use_next_core_sessions_and_screen",
    "handler::engine_neutral_handler_tests::recording_status_and_trace_attach_use_next_core_engine",
    "handler::engine_neutral_handler_tests::active_recording_export_uses_next_core_engine",
    "handler::engine_neutral_handler_tests::inactive_scrollback_export_markdown_uses_next_core_screen_engine",
    "handler::engine_neutral_handler_tests::cockpit_inbox_uses_engine_session_snapshot",
    "handler::engine_neutral_handler_tests::review_diff_does_not_require_wezterm_mux_in_next_core_mode",
    "handler::engine_neutral_handler_tests::review_rollback_requires_explicit_confirmation_at_the_mcp_boundary",
    "handler::engine_neutral_handler_tests::review_verify_and_merge_work_for_next_core_fleet_member",
    "handler::engine_neutral_handler_tests::fleet_lifecycle_uses_next_core_session_engine",
    # A call that names no pane means the caller's own, not whichever one the
    # user is looking at. Pinned by name as well as by count: the count alone
    # would go on being satisfied by any other test taking its place.
    "handler::engine_neutral_handler_tests::an_unnamed_pane_means_the_one_the_caller_speaks_from"
)

Push-Location $RepoRoot
try {
    $ErrorActionPreference = "Continue"
    $list = @(& cargo test -p unterm-mcp $TestFilter -- --list 2>&1 | ForEach-Object { $_.ToString() })
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        throw "cargo test list failed:`n$($list -join "`n")"
    }

    foreach ($test in $RequiredTests) {
        if (-not @($list | Where-Object { $_ -like "$test`: test*" })) {
            throw "missing required next-core MCP contract test: $test"
        }
    }

    if ($ListOnly) {
        Write-Host "next-core MCP contract tests present: $($RequiredTests.Count)"
        exit 0
    }

    $ErrorActionPreference = "Continue"
    $run = @(& cargo test -p unterm-mcp $TestFilter -- --test-threads=1 2>&1 | ForEach-Object { $_.ToString() })
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        throw "next-core MCP contract tests failed:`n$($run -join "`n")"
    }
    if (-not @($run | Where-Object { $_ -match "test result: ok\..*50 passed" })) {
        throw "next-core MCP contract test run did not report 50 passed tests"
    }

    # The other half of capture.scrollback: the surface refuses without a
    # front end (above), and a front end hosting it really renders (here).
    $ErrorActionPreference = "Continue"
    # --lib, not --bin: 0.68 split unterm-app's main.rs into a lib.rs and the
    # tests went with it, so `--bin unterm` has held zero tests since. The
    # count assertion below is what caught it -- had this gate asserted "at
    # least one" it would have gone on passing while testing nothing.
    $hostRun = @(& cargo test -p unterm-app --lib "mcp_host::tests::" -- --test-threads=1 2>&1 | ForEach-Object { $_.ToString() })
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        throw "MCP host tests failed:`n$($hostRun -join "`n")"
    }
    if (-not @($hostRun | Where-Object { $_ -match "test result: ok\..*4 passed" })) {
        throw "MCP host test run did not report 4 passed tests"
    }

    Write-Host "next-core MCP contract tests ok: required=$($RequiredTests.Count) module_tests=50 host_tests=4"
} finally {
    Pop-Location
}
