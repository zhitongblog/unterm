param(
    [switch]$ListOnly
)

$ErrorActionPreference = "Stop"

$CiDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $CiDir "..")

$TestFilter = "screen_buffer_"
$ExpectedCount = 115
$RequiredTests = @(
    "next_core::tests::screen_buffer_tracks_wide_character_cells",
    "next_core::tests::screen_buffer_preserves_combining_marks_on_base_cells",
    "next_core::tests::screen_buffer_attaches_combining_marks_to_previous_wide_cell",
    "next_core::tests::screen_buffer_preserves_emoji_variation_selector_width",
    "next_core::tests::screen_buffer_keeps_zwj_emoji_sequence_in_one_wide_cell",
    "next_core::tests::screen_buffer_keeps_emoji_modifier_in_base_wide_cell",
    "next_core::tests::screen_buffer_keeps_regional_indicator_flag_in_one_wide_cell",
    "next_core::tests::screen_buffer_wraps_wide_cells_before_right_edge",
    "next_core::tests::screen_buffer_repeats_wide_character_with_rep",
    "next_core::tests::screen_buffer_clears_the_right_half_when_a_wide_cell_is_overwritten",
    "next_core::tests::screen_buffer_clears_the_left_half_when_a_wide_cell_is_overwritten",
    "next_core::tests::screen_buffer_clears_the_left_half_when_a_line_is_erased_mid_wide_cell",
    "next_core::tests::screen_buffer_clears_the_left_half_when_chars_are_erased_mid_wide_cell",
    "next_core::tests::screen_buffer_splits_a_wide_cell_when_chars_are_inserted_mid_cell",
    "next_core::tests::screen_buffer_splits_a_wide_cell_pushed_past_the_right_edge_by_inserted_chars",
    "next_core::tests::screen_buffer_splits_a_wide_cell_when_chars_are_deleted_mid_cell",
    "next_core::tests::screen_buffer_splits_a_wide_cell_when_the_deleted_range_ends_mid_cell",
    "next_core::tests::screen_buffer_keeps_wide_pairs_whole_when_a_shift_moves_them_together",
    "next_core::tests::screen_buffer_splits_a_wide_cell_when_insert_mode_shifts_the_row",
    "next_core::tests::screen_buffer_splits_a_wide_cell_pushed_off_the_row_by_insert_mode",
    "next_core::tests::screen_buffer_splits_a_wide_cell_when_the_row_scrolls_left",
    "next_core::tests::screen_buffer_splits_a_wide_cell_pushed_past_the_right_edge_by_scrolling_right",
    "next_core::tests::screen_buffer_scrolls_left_across_every_row_of_the_region",
    "next_core::tests::screen_buffer_scrolls_right_across_every_row_of_the_region",
    "next_core::tests::screen_buffer_limits_horizontal_scroll_to_the_scroll_region"
)

Push-Location $RepoRoot
try {
    $ErrorActionPreference = "Continue"
    $list = @(& cargo test -p unterm-engine $TestFilter -- --list 2>&1 | ForEach-Object { $_.ToString() })
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        throw "cargo test list failed:`n$($list -join "`n")"
    }

    foreach ($test in $RequiredTests) {
        if (-not @($list | Where-Object { $_ -like "$test`: test*" })) {
            throw "missing required next-core Unicode width test: $test"
        }
    }

    if ($ListOnly) {
        Write-Host "next-core Unicode width tests present: required=$($RequiredTests.Count)"
        exit 0
    }

    $ErrorActionPreference = "Continue"
    $run = @(& cargo test -p unterm-engine $TestFilter -- --test-threads=1 2>&1 | ForEach-Object { $_.ToString() })
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        throw "next-core Unicode width tests failed:`n$($run -join "`n")"
    }
    if (-not @($run | Where-Object { $_ -match "test result: ok\..*$ExpectedCount passed" })) {
        throw "next-core Unicode width test run did not report $ExpectedCount passed tests"
    }

    Write-Host "next-core Unicode width tests ok: required=$($RequiredTests.Count) module_tests=$ExpectedCount"
} finally {
    Pop-Location
}
