#![allow(dead_code)]

use crate::{CreateSessionRequest, ScrollbackTextRequest, SplitSessionRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::next_core) enum RuntimeCommandClass {
    SessionLifecycle,
    SessionQuery,
    Input,
    ScreenRead,
    ScreenMutation,
    Recording,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::next_core) enum RuntimeCommandLane {
    Lifecycle,
    Input,
    Render,
    Screen,
    Background,
}

impl RuntimeCommandLane {
    pub(in crate::next_core) fn label(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Input => "input",
            Self::Render => "render",
            Self::Screen => "screen",
            Self::Background => "background",
        }
    }
}

#[derive(Debug)]
pub(in crate::next_core) enum RuntimeCommand {
    CreateSession(Box<CreateSessionRequest>),
    SplitSession(SplitSessionRequest),
    ListSessions,
    GetSession {
        pane_id: usize,
    },
    FocusSession {
        pane_id: usize,
    },
    ResizeSession {
        pane_id: usize,
        cols: usize,
        rows: usize,
    },
    DestroySession {
        pane_id: usize,
    },
    /// Where the divider of this pane's split now sits, so an arrangement
    /// outlives the front end that was dragging it.
    SetSplitRatio {
        pane_id: usize,
        first_ratio: f64,
    },
    WriteInput {
        pane_id: usize,
        text: String,
    },
    PasteInput {
        pane_id: usize,
        text: String,
    },
    /// A mouse event offered to the session. Whether it reaches the PTY at
    /// all depends on the modes the application negotiated, so this carries
    /// the event rather than pre-encoded bytes.
    ReportMouse {
        pane_id: usize,
        event: crate::next_core::mouse_encoding::MouseEvent,
    },
    ScrollViewport {
        pane_id: usize,
        target: isize,
    },
    /// Move the viewport by a relative number of rows, resolved under the
    /// screen lock so a concurrent scrollback trim cannot skew the step.
    ScrollViewportBy {
        pane_id: usize,
        delta: isize,
    },
    ScrollViewportToPrompt {
        pane_id: usize,
        amount: isize,
    },
    /// Drop the scrollback, and the visible screen with it when requested.
    EraseScrollback {
        pane_id: usize,
        include_viewport: bool,
    },
    ReadScreen {
        pane_id: usize,
    },
    ReadStyledScreen {
        pane_id: usize,
    },
    ReadRenderFrame {
        pane_id: usize,
        since_revision: Option<u64>,
    },
    ReadVisibleText {
        pane_id: usize,
    },
    ReadLines {
        pane_id: usize,
        start: i64,
        count: usize,
    },
    ReadScrollback {
        pane_id: usize,
        limit: usize,
    },
    ReadScrollbackText {
        pane_id: usize,
        request: ScrollbackTextRequest,
    },
    ReadStyledScrollback {
        pane_id: usize,
        request: ScrollbackTextRequest,
    },
    SearchScreen {
        pane_id: usize,
        pattern: String,
        mode: crate::SearchMode,
        max_results: usize,
    },
    Cursor {
        pane_id: usize,
    },
    /// Terminal modes a GUI pane surfaces (mouse grab, alternate screen).
    PaneModes {
        pane_id: usize,
    },
    /// The screen revision counter, for cheap change detection.
    ScreenRevision {
        pane_id: usize,
    },
    StartRecording {
        pane_id: usize,
    },
    StopRecording {
        pane_id: usize,
    },
    RecordingStatus {
        pane_id: usize,
    },
    AttachRecordingTrace {
        pane_id: usize,
        trace_id: String,
    },
    ExportRecordingMarkdown {
        pane_id: usize,
        target_path: Option<String>,
    },
    RawOutput {
        pane_id: usize,
        from: Option<u64>,
        max_bytes: usize,
    },
    ShellSnapshot {
        pane_id: usize,
    },
    SessionActivity {
        pane_id: usize,
    },
    HealthSnapshot,
}

impl RuntimeCommand {
    pub(in crate::next_core) fn class(&self) -> RuntimeCommandClass {
        match self {
            Self::CreateSession(_)
            | Self::SplitSession(_)
            | Self::FocusSession { .. }
            | Self::ResizeSession { .. }
            | Self::SetSplitRatio { .. }
            | Self::DestroySession { .. } => RuntimeCommandClass::SessionLifecycle,
            Self::ListSessions | Self::GetSession { .. } => RuntimeCommandClass::SessionQuery,
            Self::WriteInput { .. } | Self::PasteInput { .. } | Self::ReportMouse { .. } => {
                RuntimeCommandClass::Input
            }
            Self::ScrollViewport { .. }
            | Self::ScrollViewportBy { .. }
            | Self::ScrollViewportToPrompt { .. }
            | Self::EraseScrollback { .. } => RuntimeCommandClass::ScreenMutation,
            Self::ReadScreen { .. }
            | Self::ReadStyledScreen { .. }
            | Self::ReadRenderFrame { .. }
            | Self::ReadVisibleText { .. }
            | Self::ReadLines { .. }
            | Self::ReadScrollback { .. }
            | Self::ReadScrollbackText { .. }
            | Self::ReadStyledScrollback { .. }
            | Self::SearchScreen { .. }
            | Self::Cursor { .. }
            | Self::PaneModes { .. }
            | Self::ScreenRevision { .. } => RuntimeCommandClass::ScreenRead,
            Self::StartRecording { .. }
            | Self::StopRecording { .. }
            | Self::RecordingStatus { .. }
            | Self::AttachRecordingTrace { .. }
            | Self::ExportRecordingMarkdown { .. } => RuntimeCommandClass::Recording,
            Self::RawOutput { .. }
            | Self::ShellSnapshot { .. }
            | Self::SessionActivity { .. }
            | Self::HealthSnapshot => RuntimeCommandClass::Status,
        }
    }

    pub(in crate::next_core) fn pane_id(&self) -> Option<usize> {
        match self {
            Self::CreateSession(_) | Self::ListSessions | Self::HealthSnapshot => None,
            Self::SplitSession(request) => Some(request.source_pane_id),
            Self::GetSession { pane_id }
            | Self::FocusSession { pane_id }
            | Self::ResizeSession { pane_id, .. }
            | Self::SetSplitRatio { pane_id, .. }
            | Self::DestroySession { pane_id }
            | Self::WriteInput { pane_id, .. }
            | Self::PasteInput { pane_id, .. }
            | Self::ReportMouse { pane_id, .. }
            | Self::ScrollViewport { pane_id, .. }
            | Self::ScrollViewportBy { pane_id, .. }
            | Self::ScrollViewportToPrompt { pane_id, .. }
            | Self::EraseScrollback { pane_id, .. }
            | Self::ReadScreen { pane_id }
            | Self::ReadStyledScreen { pane_id }
            | Self::ReadRenderFrame { pane_id, .. }
            | Self::ReadVisibleText { pane_id }
            | Self::ReadLines { pane_id, .. }
            | Self::ReadScrollback { pane_id, .. }
            | Self::ReadScrollbackText { pane_id, .. }
            | Self::ReadStyledScrollback { pane_id, .. }
            | Self::SearchScreen { pane_id, .. }
            | Self::Cursor { pane_id }
            | Self::PaneModes { pane_id }
            | Self::ScreenRevision { pane_id }
            | Self::StartRecording { pane_id }
            | Self::StopRecording { pane_id }
            | Self::RecordingStatus { pane_id }
            | Self::AttachRecordingTrace { pane_id, .. }
            | Self::ExportRecordingMarkdown { pane_id, .. }
            | Self::RawOutput { pane_id, .. }
            | Self::ShellSnapshot { pane_id }
            | Self::SessionActivity { pane_id } => Some(*pane_id),
        }
    }

    pub(in crate::next_core) fn lane(&self) -> RuntimeCommandLane {
        match self {
            Self::CreateSession(_)
            | Self::SplitSession(_)
            | Self::FocusSession { .. }
            | Self::ResizeSession { .. }
            | Self::SetSplitRatio { .. }
            | Self::DestroySession { .. } => RuntimeCommandLane::Lifecycle,
            Self::ListSessions | Self::GetSession { .. } => RuntimeCommandLane::Background,
            Self::WriteInput { .. } | Self::PasteInput { .. } | Self::ReportMouse { .. } => {
                RuntimeCommandLane::Input
            }
            Self::ReadRenderFrame { .. } => RuntimeCommandLane::Render,
            Self::ScrollViewport { .. }
            | Self::ScrollViewportBy { .. }
            | Self::ScrollViewportToPrompt { .. }
            | Self::EraseScrollback { .. }
            | Self::ReadScreen { .. }
            | Self::ReadStyledScreen { .. }
            | Self::ReadVisibleText { .. }
            | Self::ReadLines { .. }
            | Self::ReadScrollback { .. }
            | Self::ReadScrollbackText { .. }
            | Self::ReadStyledScrollback { .. }
            | Self::SearchScreen { .. }
            | Self::Cursor { .. }
            | Self::PaneModes { .. }
            | Self::ScreenRevision { .. } => RuntimeCommandLane::Screen,
            Self::StartRecording { .. }
            | Self::StopRecording { .. }
            | Self::RecordingStatus { .. }
            | Self::AttachRecordingTrace { .. }
            | Self::ExportRecordingMarkdown { .. }
            | Self::RawOutput { .. }
            | Self::ShellSnapshot { .. }
            | Self::SessionActivity { .. }
            | Self::HealthSnapshot => RuntimeCommandLane::Background,
        }
    }

    pub(in crate::next_core) fn is_write_path(&self) -> bool {
        matches!(
            self,
            Self::CreateSession(_)
                | Self::SplitSession(_)
                | Self::FocusSession { .. }
                | Self::ResizeSession { .. }
                | Self::SetSplitRatio { .. }
                | Self::DestroySession { .. }
                | Self::WriteInput { .. }
                | Self::PasteInput { .. }
                | Self::ReportMouse { .. }
                | Self::ScrollViewport { .. }
                | Self::ScrollViewportBy { .. }
                | Self::ScrollViewportToPrompt { .. }
                | Self::EraseScrollback { .. }
                | Self::StartRecording { .. }
                | Self::StopRecording { .. }
                | Self::AttachRecordingTrace { .. }
                | Self::ExportRecordingMarkdown { .. }
        )
    }

    pub(in crate::next_core) fn latency_sensitive(&self) -> bool {
        matches!(
            self,
            Self::WriteInput { .. }
                | Self::PasteInput { .. }
                | Self::ReportMouse { .. }
                | Self::FocusSession { .. }
                | Self::ReadRenderFrame { .. }
        )
    }

    pub(in crate::next_core) fn input_bytes(&self) -> usize {
        match self {
            Self::WriteInput { text, .. } | Self::PasteInput { text, .. } => text.len(),
            // A mouse report's real length is not known until the session's
            // modes are consulted at dispatch, so charge a conservative
            // upper bound up front. Counting zero would exempt mouse motion
            // from input backpressure entirely, and with `CSI ? 1003 h` every
            // pointer move is a report.
            Self::ReportMouse { .. } => MAX_MOUSE_REPORT_BYTES,
            _ => 0,
        }
    }
}

/// Upper bound on an encoded mouse report: `CSI < 255 ; 9999 ; 9999 M` and
/// the shorter legacy forms all fit well inside this.
pub(in crate::next_core) const MAX_MOUSE_REPORT_BYTES: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::next_core) struct RuntimeQueuePolicy {
    pub max_pending_commands: usize,
    pub max_pending_input_bytes: usize,
    pub max_render_wakeups_per_second: u16,
}

impl Default for RuntimeQueuePolicy {
    fn default() -> Self {
        Self {
            max_pending_commands: 2048,
            max_pending_input_bytes: 1024 * 1024,
            max_render_wakeups_per_second: 120,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SplitDirection;

    #[test]
    fn classifies_input_as_latency_sensitive_write_path() {
        let command = RuntimeCommand::WriteInput {
            pane_id: 7,
            text: "\u{1b}[C".to_string(),
        };

        assert_eq!(command.class(), RuntimeCommandClass::Input);
        assert_eq!(command.lane(), RuntimeCommandLane::Input);
        assert_eq!(command.pane_id(), Some(7));
        assert!(command.is_write_path());
        assert!(command.latency_sensitive());
    }

    #[test]
    fn classifies_render_reads_as_latency_sensitive_read_path() {
        let command = RuntimeCommand::ReadRenderFrame {
            pane_id: 3,
            since_revision: Some(42),
        };

        assert_eq!(command.class(), RuntimeCommandClass::ScreenRead);
        assert_eq!(command.lane(), RuntimeCommandLane::Render);
        assert_eq!(command.pane_id(), Some(3));
        assert!(!command.is_write_path());
        assert!(command.latency_sensitive());
    }

    #[test]
    fn classifies_session_lifecycle_source_pane() {
        let command = RuntimeCommand::SplitSession(SplitSessionRequest {
            source_pane_id: 9,
            direction: SplitDirection::Right,
            size_percent: 50,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        });

        assert_eq!(command.class(), RuntimeCommandClass::SessionLifecycle);
        assert_eq!(command.lane(), RuntimeCommandLane::Lifecycle);
        assert_eq!(command.pane_id(), Some(9));
        assert!(command.is_write_path());
    }

    #[test]
    fn default_queue_policy_sets_bounded_backpressure_budget() {
        let policy = RuntimeQueuePolicy::default();

        assert_eq!(policy.max_pending_commands, 2048);
        assert_eq!(policy.max_pending_input_bytes, 1024 * 1024);
        assert_eq!(policy.max_render_wakeups_per_second, 120);
    }

    #[test]
    fn reports_input_byte_cost_only_for_input_payloads() {
        let paste = RuntimeCommand::PasteInput {
            pane_id: 1,
            text: "abc".to_string(),
        };
        let read = RuntimeCommand::ReadScreen { pane_id: 1 };

        assert_eq!(paste.input_bytes(), 3);
        assert_eq!(read.input_bytes(), 0);
    }
}
