use super::{
    command::{RuntimeCommand, RuntimeCommandClass},
    consumer,
    dispatch::RuntimeDispatchResult,
};
use crate::{
    CreateSessionRequest, CursorSnapshot, EngineHealthSnapshot, RecordingExportResult,
    RecordingStartResult, RecordingStatusSnapshot, RecordingStopResult, RenderFrameSnapshot,
    ScreenLine, ScreenSearchMatch, ScreenSnapshot, ScrollbackTextRequest, ScrollbackTextSnapshot,
    SessionActivitySnapshot, SessionSnapshot, ShellSnapshot, SplitSessionRequest,
    StyledScreenSnapshot, StyledScrollbackSnapshot,
};
use anyhow::{bail, Result};

pub(in crate::next_core) fn submit_input(command: RuntimeCommand) -> Result<()> {
    if command.class() != RuntimeCommandClass::Input {
        bail!(
            "runtime scheduler expected input command, got {:?}",
            command.class()
        );
    }

    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected input dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn focus_session(pane_id: usize) -> Result<()> {
    let command = RuntimeCommand::FocusSession { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected focus-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn resize_session(pane_id: usize, cols: usize, rows: usize) -> Result<()> {
    let command = RuntimeCommand::ResizeSession {
        pane_id,
        cols,
        rows,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected resize-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn set_split_ratio(pane_id: usize, first_ratio: f64) -> Result<()> {
    let command = RuntimeCommand::SetSplitRatio {
        pane_id,
        first_ratio,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected split-ratio dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn destroy_session(pane_id: usize) -> Result<()> {
    let command = RuntimeCommand::DestroySession { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected destroy-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn create_session(
    request: CreateSessionRequest,
) -> Result<SessionSnapshot> {
    let command = RuntimeCommand::CreateSession(Box::new(request));
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Session(session) => Ok(session),
        other => bail!(
            "runtime scheduler expected create-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn split_session(request: SplitSessionRequest) -> Result<SessionSnapshot> {
    let command = RuntimeCommand::SplitSession(request);
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Session(session) => Ok(session),
        other => bail!(
            "runtime scheduler expected split-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn list_sessions() -> Result<Vec<SessionSnapshot>> {
    match consumer::submit_and_dispatch_response(RuntimeCommand::ListSessions)? {
        RuntimeDispatchResult::Sessions(sessions) => Ok(sessions),
        other => bail!(
            "runtime scheduler expected list-sessions dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn get_session(pane_id: usize) -> Result<SessionSnapshot> {
    let command = RuntimeCommand::GetSession { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Session(session) => Ok(session),
        other => bail!(
            "runtime scheduler expected get-session dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn start_recording(pane_id: usize) -> Result<RecordingStartResult> {
    let command = RuntimeCommand::StartRecording { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RecordingStart(started) => Ok(started),
        other => bail!(
            "runtime scheduler expected start-recording dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn stop_recording(pane_id: usize) -> Result<RecordingStopResult> {
    let command = RuntimeCommand::StopRecording { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RecordingStop(stopped) => Ok(stopped),
        other => bail!(
            "runtime scheduler expected stop-recording dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn recording_status(pane_id: usize) -> Result<RecordingStatusSnapshot> {
    let command = RuntimeCommand::RecordingStatus { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RecordingStatus(status) => Ok(status),
        other => bail!(
            "runtime scheduler expected recording-status dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn attach_recording_trace(
    pane_id: usize,
    trace_id: String,
) -> Result<Vec<String>> {
    let command = RuntimeCommand::AttachRecordingTrace { pane_id, trace_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RecordingTraceIds(trace_ids) => Ok(trace_ids),
        other => bail!(
            "runtime scheduler expected attach-recording-trace dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn export_recording_markdown(
    pane_id: usize,
    target_path: Option<String>,
) -> Result<RecordingExportResult> {
    let command = RuntimeCommand::ExportRecordingMarkdown {
        pane_id,
        target_path,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RecordingExport(export) => Ok(export),
        other => bail!(
            "runtime scheduler expected export-recording-markdown dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_render_frame(
    pane_id: usize,
    since_revision: Option<u64>,
) -> Result<RenderFrameSnapshot> {
    let command = RuntimeCommand::ReadRenderFrame {
        pane_id,
        since_revision,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::RenderFrame(frame) => Ok(frame),
        other => bail!(
            "runtime scheduler expected render-frame dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn scroll_viewport_to(pane_id: usize, target: isize) -> Result<()> {
    let command = RuntimeCommand::ScrollViewport { pane_id, target };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected screen-mutation dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn scroll_viewport_by(pane_id: usize, delta: isize) -> Result<()> {
    let command = RuntimeCommand::ScrollViewportBy { pane_id, delta };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected screen-mutation dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn scroll_viewport_to_prompt(pane_id: usize, amount: isize) -> Result<()> {
    let command = RuntimeCommand::ScrollViewportToPrompt { pane_id, amount };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected screen-mutation dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_screen(pane_id: usize) -> Result<ScreenSnapshot> {
    let command = RuntimeCommand::ReadScreen { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Screen(screen) => Ok(screen),
        other => bail!(
            "runtime scheduler expected plain screen dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_styled_screen(pane_id: usize) -> Result<StyledScreenSnapshot> {
    let command = RuntimeCommand::ReadStyledScreen { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::StyledScreen(screen) => Ok(screen),
        other => bail!(
            "runtime scheduler expected styled screen dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_visible_text(pane_id: usize) -> Result<String> {
    let command = RuntimeCommand::ReadVisibleText { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::VisibleText(text) => Ok(text),
        other => bail!(
            "runtime scheduler expected visible-text dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_lines(
    pane_id: usize,
    start: i64,
    count: usize,
) -> Result<Vec<ScreenLine>> {
    let command = RuntimeCommand::ReadLines {
        pane_id,
        start,
        count,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Lines(lines) => Ok(lines),
        other => bail!(
            "runtime scheduler expected line-range dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_scrollback(pane_id: usize, limit: usize) -> Result<Vec<String>> {
    let command = RuntimeCommand::ReadScrollback { pane_id, limit };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Scrollback(lines) => Ok(lines),
        other => bail!(
            "runtime scheduler expected scrollback dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_scrollback_text(
    pane_id: usize,
    request: ScrollbackTextRequest,
) -> Result<ScrollbackTextSnapshot> {
    let command = RuntimeCommand::ReadScrollbackText { pane_id, request };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::ScrollbackText(snapshot) => Ok(snapshot),
        other => bail!(
            "runtime scheduler expected scrollback-text dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn read_styled_scrollback(
    pane_id: usize,
    request: ScrollbackTextRequest,
) -> Result<StyledScrollbackSnapshot> {
    let command = RuntimeCommand::ReadStyledScrollback { pane_id, request };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::StyledScrollback(snapshot) => Ok(snapshot),
        other => bail!(
            "runtime scheduler expected styled-scrollback dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn search_screen(
    pane_id: usize,
    pattern: &str,
    mode: crate::SearchMode,
    max_results: usize,
) -> Result<Vec<ScreenSearchMatch>> {
    let command = RuntimeCommand::SearchScreen {
        pane_id,
        pattern: pattern.to_string(),
        mode,
        max_results,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Search(matches) => Ok(matches),
        other => bail!(
            "runtime scheduler expected search dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn cursor(pane_id: usize) -> Result<CursorSnapshot> {
    let command = RuntimeCommand::Cursor { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Cursor(cursor) => Ok(cursor),
        other => bail!(
            "runtime scheduler expected cursor dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn pane_modes(pane_id: usize) -> Result<crate::PaneModesSnapshot> {
    let command = RuntimeCommand::PaneModes { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::PaneModes(modes) => Ok(modes),
        other => bail!(
            "runtime scheduler expected pane-modes dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn erase_scrollback(pane_id: usize, include_viewport: bool) -> Result<()> {
    let command = RuntimeCommand::EraseScrollback {
        pane_id,
        include_viewport,
    };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Unit => Ok(()),
        other => bail!(
            "runtime scheduler expected screen-mutation dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn screen_revision(pane_id: usize) -> Result<u64> {
    let command = RuntimeCommand::ScreenRevision { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::ScreenRevision(revision) => Ok(revision),
        other => bail!(
            "runtime scheduler expected screen-revision dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn output(
    pane_id: usize,
    from: Option<u64>,
    max_bytes: usize,
) -> Result<crate::OutputSince> {
    let command = RuntimeCommand::RawOutput { pane_id, from, max_bytes };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::Output(output) => Ok(output),
        other => bail!(
            "runtime scheduler expected raw-output dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn shell_snapshot(pane_id: usize) -> Result<ShellSnapshot> {
    let command = RuntimeCommand::ShellSnapshot { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::ShellSnapshot(shell) => Ok(shell),
        other => bail!(
            "runtime scheduler expected shell-snapshot dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn session_activity(pane_id: usize) -> Result<SessionActivitySnapshot> {
    let command = RuntimeCommand::SessionActivity { pane_id };
    match consumer::submit_and_dispatch_response(command)? {
        RuntimeDispatchResult::SessionActivity(activity) => Ok(activity),
        other => bail!(
            "runtime scheduler expected session-activity dispatch result, got {:?}",
            other
        ),
    }
}

pub(in crate::next_core) fn health_snapshot() -> Result<EngineHealthSnapshot> {
    match consumer::submit_and_dispatch_response(RuntimeCommand::HealthSnapshot)? {
        RuntimeDispatchResult::HealthSnapshot(health) => Ok(health),
        other => bail!(
            "runtime scheduler expected health-snapshot dispatch result, got {:?}",
            other
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_core::runtime::{
        command::RuntimeQueuePolicy, consumer, queue::RuntimeQueueStats, test_facade, with_current,
        with_current_mut,
    };

    fn queue_stats() -> RuntimeQueueStats {
        with_current(|state| state.command_queue.stats())
    }

    fn install_zero_command_budget() {
        with_current_mut(|state| {
            state.command_queue =
                super::super::queue::RuntimeCommandQueue::new(RuntimeQueuePolicy {
                    max_pending_commands: 0,
                    max_pending_input_bytes: 1024,
                    max_render_wakeups_per_second: 120,
                });
        });
    }

    #[test]
    fn runtime_owns_command_queue_stats() {
        let _runtime = test_facade::reset();

        assert_eq!(queue_stats(), RuntimeQueueStats::default());
    }

    #[test]
    fn submit_input_rejects_non_input_before_queueing() {
        let _runtime = test_facade::reset();

        let err = submit_input(RuntimeCommand::ReadScreen { pane_id: 1 })
            .expect_err("non-input command should be rejected");

        assert!(err.to_string().contains("expected input command"));
        assert_eq!(queue_stats(), RuntimeQueueStats::default());
    }

    #[test]
    fn submit_input_uses_command_backpressure() {
        let _runtime = test_facade::reset();

        install_zero_command_budget();

        let err = submit_input(RuntimeCommand::WriteInput {
            pane_id: 1,
            text: "x".to_string(),
        })
        .expect_err("zero command budget should reject input");

        assert!(err
            .to_string()
            .contains("runtime input queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn submit_input_uses_input_byte_backpressure() {
        let _runtime = test_facade::reset();

        with_current_mut(|state| {
            state.command_queue =
                super::super::queue::RuntimeCommandQueue::new(RuntimeQueuePolicy {
                    max_pending_commands: 4,
                    max_pending_input_bytes: 2,
                    max_render_wakeups_per_second: 120,
                });
        });

        let err = submit_input(RuntimeCommand::PasteInput {
            pane_id: 1,
            text: "abc".to_string(),
        })
        .expect_err("input larger than budget should be rejected");

        assert!(err
            .to_string()
            .contains("runtime input queue rejected command"));
        assert!(err.to_string().contains("InputBackpressure"));
        assert_eq!(queue_stats().rejected_input_bytes, 3);
    }

    #[test]
    fn submit_input_dispatches_before_older_screen_backlog() {
        let _runtime = test_facade::reset();
        with_current_mut(|state| {
            state
                .command_queue
                .enqueue(RuntimeCommand::ReadScreen { pane_id: 404 })
                .unwrap();
        });

        let err = submit_input(RuntimeCommand::WriteInput {
            pane_id: 404,
            text: "x".to_string(),
        })
        .expect_err("missing input pane should fail after input dispatch");

        assert!(err.to_string().contains("next-core session 404 not found"));
        let stats = queue_stats();
        assert_eq!(stats.pending_lanes.input, 0);
        assert_eq!(stats.pending_lanes.screen, 1);
    }

    #[test]
    fn lifecycle_mutations_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = focus_session(404).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn lifecycle_mutations_use_lifecycle_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = destroy_session(1).expect_err("zero command budget should reject destroy");

        assert!(err
            .to_string()
            .contains("runtime lifecycle queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn session_creation_uses_lifecycle_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })
        .expect_err("zero command budget should reject create");

        assert!(err
            .to_string()
            .contains("runtime lifecycle queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn split_session_enters_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = split_session(SplitSessionRequest {
            source_pane_id: 404,
            direction: crate::SplitDirection::Right,
            size_percent: 50,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })
        .expect_err("missing split source should fail after dispatch");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn lifecycle_dispatches_before_older_render_and_screen_backlog() {
        let _runtime = test_facade::reset();
        with_current_mut(|state| {
            state
                .command_queue
                .enqueue(RuntimeCommand::ReadRenderFrame {
                    pane_id: 404,
                    since_revision: None,
                })
                .unwrap();
            state
                .command_queue
                .enqueue(RuntimeCommand::ReadScreen { pane_id: 404 })
                .unwrap();
        });

        let err = resize_session(404, 80, 24).expect_err("missing pane should fail after resize");

        assert!(err.to_string().contains("next-core session 404 not found"));
        let stats = queue_stats();
        assert_eq!(stats.pending_lanes.lifecycle, 0);
        assert_eq!(stats.pending_lanes.render, 1);
        assert_eq!(stats.pending_lanes.screen, 1);
    }

    #[test]
    fn session_queries_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let sessions = list_sessions().expect("list should dispatch through runtime queue");

        assert!(sessions.is_empty());
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn session_queries_use_background_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = get_session(1).expect_err("zero command budget should reject get-session");

        assert!(err
            .to_string()
            .contains("runtime background queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn session_query_backlog_waits_behind_input() {
        let _runtime = test_facade::reset();
        with_current_mut(|state| {
            state
                .command_queue
                .enqueue(RuntimeCommand::ListSessions)
                .unwrap();
        });

        let err = submit_input(RuntimeCommand::WriteInput {
            pane_id: 404,
            text: "x".to_string(),
        })
        .expect_err("missing input pane should fail first");

        assert!(err.to_string().contains("next-core session 404 not found"));
        let stats = queue_stats();
        assert_eq!(stats.pending_lanes.input, 0);
        assert_eq!(stats.pending_lanes.background, 1);
    }

    #[test]
    fn recording_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let status = recording_status(404).expect("missing recording status is inactive");

        assert!(!status.enabled);
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn recording_commands_use_background_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = recording_status(1).expect_err("zero command budget should reject recording");

        assert!(err
            .to_string()
            .contains("runtime background queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn recording_backlog_waits_behind_lifecycle() {
        let _runtime = test_facade::reset();
        with_current_mut(|state| {
            state
                .command_queue
                .enqueue(RuntimeCommand::RecordingStatus { pane_id: 404 })
                .unwrap();
        });

        let err = focus_session(404).expect_err("missing lifecycle pane should fail first");

        assert!(err.to_string().contains("next-core session 404 not found"));
        let stats = queue_stats();
        assert_eq!(stats.pending_lanes.lifecycle, 0);
        assert_eq!(stats.pending_lanes.background, 1);
    }

    #[test]
    fn render_frame_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_render_frame(404, Some(7)).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn render_frame_reads_use_command_backpressure() {
        let _runtime = test_facade::reset();

        install_zero_command_budget();

        let err = read_render_frame(1, None).expect_err("zero command budget should reject read");

        assert!(err
            .to_string()
            .contains("runtime render queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn plain_screen_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_screen(404).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn plain_screen_reads_use_command_backpressure() {
        let _runtime = test_facade::reset();

        install_zero_command_budget();

        let err = read_screen(1).expect_err("zero command budget should reject read");

        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn styled_screen_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_styled_screen(404).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn styled_screen_reads_use_command_backpressure() {
        let _runtime = test_facade::reset();

        install_zero_command_budget();

        let err = read_styled_screen(1).expect_err("zero command budget should reject read");

        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn visible_text_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_visible_text(404).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn remaining_screen_reads_use_command_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = read_visible_text(1).expect_err("zero command budget should reject visible text");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = read_lines(1, 0, 1).expect_err("zero command budget should reject line read");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = read_scrollback(1, 1).expect_err("zero command budget should reject scrollback");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = read_scrollback_text(
            1,
            ScrollbackTextRequest {
                start_line: None,
                end_line: None,
                tail_lines: Some(1),
                escapes: false,
            },
        )
        .expect_err("zero command budget should reject scrollback text");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = read_styled_scrollback(
            1,
            ScrollbackTextRequest {
                start_line: None,
                end_line: None,
                tail_lines: Some(1),
                escapes: false,
            },
        )
        .expect_err("zero command budget should reject styled scrollback");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = search_screen(1, "needle", crate::SearchMode::CaseSensitive, 1)
            .expect_err("zero command budget should reject search");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));

        _runtime.reset();
        install_zero_command_budget();
        let err = cursor(1).expect_err("zero command budget should reject cursor read");
        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));
    }

    #[test]
    fn line_range_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_lines(404, 0, 1).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn scrollback_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_scrollback(404, 10).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn viewport_scrolls_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = scroll_viewport_to(404, 5).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn viewport_scrolls_use_command_backpressure() {
        let _runtime = test_facade::reset();

        install_zero_command_budget();

        let err = scroll_viewport_to(1, 5).expect_err("zero command budget should reject scroll");

        assert!(err
            .to_string()
            .contains("runtime screen queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn viewport_scrolls_dispatch_before_background_backlog() {
        let _runtime = test_facade::reset();
        with_current_mut(|state| {
            state
                .command_queue
                .enqueue(RuntimeCommand::HealthSnapshot)
                .unwrap();
        });

        let err = scroll_viewport_to(404, 5).expect_err("missing pane should fail after scroll");

        assert!(err.to_string().contains("next-core session 404 not found"));
        let stats = queue_stats();
        assert_eq!(stats.pending_lanes.screen, 0);
        assert_eq!(stats.pending_lanes.background, 1);
    }

    #[test]
    fn status_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let health = health_snapshot().expect("health should dispatch through runtime queue");

        assert_eq!(health.engine, "next-core");
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn status_reads_use_background_backpressure() {
        let _runtime = test_facade::reset();
        install_zero_command_budget();

        let err = shell_snapshot(1).expect_err("zero command budget should reject shell status");

        assert!(err
            .to_string()
            .contains("runtime background queue rejected command"));
        assert_eq!(queue_stats().rejected_commands, 1);
    }

    #[test]
    fn scrollback_text_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_scrollback_text(
            404,
            ScrollbackTextRequest {
                start_line: None,
                end_line: None,
                tail_lines: Some(10),
                escapes: false,
            },
        )
        .expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn styled_scrollback_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = read_styled_scrollback(
            404,
            ScrollbackTextRequest {
                start_line: None,
                end_line: None,
                tail_lines: Some(10),
                escapes: false,
            },
        )
        .expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn screen_search_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = search_screen(404, "needle", crate::SearchMode::CaseSensitive, 5)
            .expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn cursor_reads_enter_runtime_queue_before_dispatch() {
        let _runtime = test_facade::reset();

        let err = cursor(404).expect_err("missing pane should fail");

        assert!(err.to_string().contains("next-core session 404 not found"));
        assert_eq!(queue_stats().pending_commands, 0);
        assert_eq!(queue_stats().rejected_commands, 0);
    }

    #[test]
    fn enqueue_dequeue_updates_runtime_queue_backpressure_state() {
        let _runtime = test_facade::reset();

        with_current_mut(|state| {
            state.command_queue =
                super::super::queue::RuntimeCommandQueue::new(RuntimeQueuePolicy {
                    max_pending_commands: 4,
                    max_pending_input_bytes: 2,
                    max_render_wakeups_per_second: 120,
                });
        });

        let err = consumer::consume_sync(RuntimeCommand::PasteInput {
            pane_id: 1,
            text: "abc".to_string(),
        })
        .expect_err("input larger than budget should be rejected");

        assert!(err
            .to_string()
            .contains("runtime input queue rejected command"));
        assert!(err.to_string().contains("InputBackpressure"));
        assert_eq!(queue_stats().rejected_input_bytes, 3);
    }
}
