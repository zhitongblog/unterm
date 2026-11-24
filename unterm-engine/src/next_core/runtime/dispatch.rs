#![allow(dead_code)]

use super::{
    command::{RuntimeCommand, RuntimeCommandClass},
    input_executor, recording_executor, screen_executor, session_executor, session_query_executor,
    status_executor,
};
use crate::{
    CursorSnapshot, EngineHealthSnapshot, RecordingExportResult, RecordingStartResult,
    RecordingStatusSnapshot, RecordingStopResult, RenderFrameSnapshot, ScreenLine,
    ScreenSearchMatch, ScreenSnapshot, ScrollbackTextSnapshot, SessionActivitySnapshot,
    SessionSnapshot, ShellSnapshot, StyledScreenSnapshot, StyledScrollbackSnapshot,
};
use anyhow::{bail, Result};

#[derive(Debug)]
pub(in crate::next_core) enum RuntimeDispatchResult {
    Unit,
    Screen(ScreenSnapshot),
    StyledScreen(StyledScreenSnapshot),
    RenderFrame(RenderFrameSnapshot),
    VisibleText(String),
    Lines(Vec<ScreenLine>),
    Scrollback(Vec<String>),
    ScrollbackText(ScrollbackTextSnapshot),
    StyledScrollback(StyledScrollbackSnapshot),
    Search(Vec<ScreenSearchMatch>),
    Cursor(CursorSnapshot),
    PaneModes(crate::PaneModesSnapshot),
    ScreenRevision(u64),
    Output(crate::OutputSince),
    ShellSnapshot(ShellSnapshot),
    SessionActivity(SessionActivitySnapshot),
    HealthSnapshot(EngineHealthSnapshot),
    RecordingStart(RecordingStartResult),
    RecordingStop(RecordingStopResult),
    RecordingStatus(RecordingStatusSnapshot),
    RecordingTraceIds(Vec<String>),
    RecordingExport(RecordingExportResult),
    Sessions(Vec<SessionSnapshot>),
    Session(SessionSnapshot),
}

pub(in crate::next_core) fn execute(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command.class() {
        RuntimeCommandClass::Input => {
            input_executor::execute(command)?;
            Ok(RuntimeDispatchResult::Unit)
        }
        RuntimeCommandClass::ScreenMutation => {
            screen_executor::execute_screen_mutation(command)?;
            Ok(RuntimeDispatchResult::Unit)
        }
        RuntimeCommandClass::SessionLifecycle => execute_session_lifecycle(command),
        RuntimeCommandClass::SessionQuery => execute_session_query(command),
        RuntimeCommandClass::ScreenRead => execute_screen_read(command),
        RuntimeCommandClass::Status => execute_status(command),
        RuntimeCommandClass::Recording => execute_recording(command),
    }
}

fn execute_session_lifecycle(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command {
        RuntimeCommand::CreateSession(_) => Ok(RuntimeDispatchResult::Session(
            session_executor::execute_create(command)?,
        )),
        RuntimeCommand::SplitSession(_) => Ok(RuntimeDispatchResult::Session(
            session_executor::execute_split(command)?,
        )),
        RuntimeCommand::FocusSession { .. }
        | RuntimeCommand::ResizeSession { .. }
        | RuntimeCommand::SetSplitRatio { .. }
        | RuntimeCommand::DestroySession { .. } => {
            session_executor::execute_mutation(command)?;
            Ok(RuntimeDispatchResult::Unit)
        }
        _ => bail!("runtime dispatch expected session lifecycle command"),
    }
}

fn execute_session_query(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command {
        RuntimeCommand::ListSessions => Ok(RuntimeDispatchResult::Sessions(
            session_query_executor::execute_list(command)?,
        )),
        RuntimeCommand::GetSession { .. } => Ok(RuntimeDispatchResult::Session(
            session_query_executor::execute_get(command)?,
        )),
        _ => bail!("runtime dispatch expected session query command"),
    }
}

fn execute_recording(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command {
        RuntimeCommand::StartRecording { .. } => Ok(RuntimeDispatchResult::RecordingStart(
            recording_executor::execute_start(command)?,
        )),
        RuntimeCommand::StopRecording { .. } => Ok(RuntimeDispatchResult::RecordingStop(
            recording_executor::execute_stop(command)?,
        )),
        RuntimeCommand::RecordingStatus { .. } => Ok(RuntimeDispatchResult::RecordingStatus(
            recording_executor::execute_status(command)?,
        )),
        RuntimeCommand::AttachRecordingTrace { .. } => {
            Ok(RuntimeDispatchResult::RecordingTraceIds(
                recording_executor::execute_attach_trace(command)?,
            ))
        }
        RuntimeCommand::ExportRecordingMarkdown { .. } => {
            Ok(RuntimeDispatchResult::RecordingExport(
                recording_executor::execute_export_markdown(command)?,
            ))
        }
        _ => bail!("runtime dispatch expected recording command"),
    }
}

fn execute_status(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command {
        RuntimeCommand::RawOutput { .. } => Ok(RuntimeDispatchResult::Output(
            status_executor::execute_output(command)?,
        )),
        RuntimeCommand::ShellSnapshot { .. } => Ok(RuntimeDispatchResult::ShellSnapshot(
            status_executor::execute_shell_snapshot(command)?,
        )),
        RuntimeCommand::SessionActivity { .. } => Ok(RuntimeDispatchResult::SessionActivity(
            status_executor::execute_session_activity(command)?,
        )),
        RuntimeCommand::HealthSnapshot => Ok(RuntimeDispatchResult::HealthSnapshot(
            status_executor::execute_health_snapshot(command)?,
        )),
        _ => bail!("runtime dispatch expected status command"),
    }
}

fn execute_screen_read(command: RuntimeCommand) -> Result<RuntimeDispatchResult> {
    match command {
        RuntimeCommand::ReadScreen { .. } => Ok(RuntimeDispatchResult::Screen(
            screen_executor::execute_screen(command)?,
        )),
        RuntimeCommand::ReadStyledScreen { .. } => Ok(RuntimeDispatchResult::StyledScreen(
            screen_executor::execute_styled_screen(command)?,
        )),
        RuntimeCommand::ReadRenderFrame { .. } => Ok(RuntimeDispatchResult::RenderFrame(
            screen_executor::execute_render_frame(command)?,
        )),
        RuntimeCommand::ReadVisibleText { .. } => Ok(RuntimeDispatchResult::VisibleText(
            screen_executor::execute_visible_text(command)?,
        )),
        RuntimeCommand::ReadLines { .. } => Ok(RuntimeDispatchResult::Lines(
            screen_executor::execute_lines(command)?,
        )),
        RuntimeCommand::ReadScrollback { .. } => Ok(RuntimeDispatchResult::Scrollback(
            screen_executor::execute_scrollback(command)?,
        )),
        RuntimeCommand::ReadScrollbackText { .. } => Ok(RuntimeDispatchResult::ScrollbackText(
            screen_executor::execute_scrollback_text(command)?,
        )),
        RuntimeCommand::ReadStyledScrollback { .. } => Ok(RuntimeDispatchResult::StyledScrollback(
            screen_executor::execute_styled_scrollback(command)?,
        )),
        RuntimeCommand::SearchScreen { .. } => Ok(RuntimeDispatchResult::Search(
            screen_executor::execute_search(command)?,
        )),
        RuntimeCommand::Cursor { .. } => Ok(RuntimeDispatchResult::Cursor(
            screen_executor::execute_cursor(command)?,
        )),
        RuntimeCommand::PaneModes { .. } => Ok(RuntimeDispatchResult::PaneModes(
            screen_executor::execute_pane_modes(command)?,
        )),
        RuntimeCommand::ScreenRevision { .. } => Ok(RuntimeDispatchResult::ScreenRevision(
            screen_executor::execute_screen_revision(command)?,
        )),
        _ => bail!("runtime dispatch expected screen read command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_core::runtime::test_facade;

    #[test]
    fn dispatch_rejects_wrong_recording_shape() {
        let err = super::execute_recording(RuntimeCommand::HealthSnapshot)
            .expect_err("wrong command should fail");

        assert!(err.to_string().contains("expected recording command"));
    }

    #[test]
    fn dispatch_rejects_wrong_lifecycle_shape() {
        let err = super::execute_session_lifecycle(RuntimeCommand::HealthSnapshot)
            .expect_err("wrong command should fail");

        assert!(err
            .to_string()
            .contains("expected session lifecycle command"));
    }

    #[test]
    fn dispatch_routes_recording_commands_to_recording_executor() {
        let _runtime = test_facade::reset();

        let result = execute(RuntimeCommand::RecordingStatus { pane_id: 404 })
            .expect("missing recording status should be inactive");

        assert!(matches!(result, RuntimeDispatchResult::RecordingStatus(_)));
    }

    #[test]
    fn dispatch_routes_lifecycle_commands_to_session_executor() {
        let _runtime = test_facade::reset();

        let err = execute(RuntimeCommand::FocusSession { pane_id: 404 })
            .expect_err("missing pane should come from session executor");

        assert!(err.to_string().contains("next-core session 404 not found"));
    }

    #[test]
    fn dispatch_routes_split_session_to_lifecycle_response() {
        let _runtime = test_facade::reset();

        let err = execute(RuntimeCommand::SplitSession(crate::SplitSessionRequest {
            source_pane_id: 404,
            direction: crate::SplitDirection::Right,
            size_percent: 50,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        }))
        .expect_err("missing split source should come from lifecycle executor");

        assert!(err.to_string().contains("next-core session 404 not found"));
    }

    #[test]
    fn dispatch_routes_session_queries_to_session_query_executor() {
        let _runtime = test_facade::reset();

        let result = execute(RuntimeCommand::ListSessions).expect("list should dispatch");

        assert!(matches!(result, RuntimeDispatchResult::Sessions(_)));
    }

    #[test]
    fn dispatch_routes_status_commands_to_status_executor() {
        let _runtime = test_facade::reset();

        let result = execute(RuntimeCommand::HealthSnapshot).expect("health should dispatch");

        assert!(matches!(result, RuntimeDispatchResult::HealthSnapshot(_)));
    }

    #[test]
    fn dispatch_routes_screen_reads_to_screen_executor() {
        let _runtime = test_facade::reset();

        let err = execute(RuntimeCommand::ReadScreen { pane_id: 404 })
            .expect_err("missing pane should still come from screen executor");

        assert!(err.to_string().contains("next-core session 404 not found"));
    }
}
