use super::{
    activity::SessionIoActivity, pty_io, recording_output, terminal_queries, NextCoreRecording,
    NextCoreScreen, MAX_OUTPUT_BYTES,
};
use parking_lot::Mutex;
use std::{io::Write, sync::Arc, time::Instant};

pub(super) struct OutputHandles<'a> {
    pub(super) output: &'a Arc<Mutex<String>>,
    pub(super) screen: &'a Arc<Mutex<NextCoreScreen>>,
    pub(super) recording: &'a Arc<Mutex<Option<NextCoreRecording>>>,
    pub(super) activity: &'a Arc<Mutex<SessionIoActivity>>,
    pub(super) writer: &'a Arc<Mutex<Box<dyn Write + Send>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OutputApplyStats {
    pub(super) input_bytes: usize,
    pub(super) terminal_response_bytes: usize,
    pub(super) recorded: bool,
}

pub(super) fn apply_chunk(
    handles: OutputHandles<'_>,
    chunk: &str,
    pending_terminal_query: &mut String,
) -> OutputApplyStats {
    let chunk = pty_io::strip_powershell_startup_noise(chunk);
    let chunk = chunk.as_ref();
    let started_at = Instant::now();
    {
        let mut output = handles.output.lock();
        pty_io::append_bounded_output(&mut output, chunk, MAX_OUTPUT_BYTES);
    }

    {
        let mut screen = handles.screen.lock();
        screen.feed(chunk);
        let mut terminal_response_bytes = terminal_queries::answer_with_pending(
            chunk,
            &screen,
            handles.writer,
            pending_terminal_query,
        );
        // Picture commands answer from inside the parser (kitty's `OK` and
        // errors): sent after the chunk that asked, like the queries above.
        let replies = screen.take_replies();
        drop(screen);
        if !replies.is_empty() {
            let mut writer = handles.writer.lock();
            if writer.write_all(&replies).and_then(|_| writer.flush()).is_ok() {
                terminal_response_bytes += replies.len();
            }
        }

        let recorded = if let Some(recording) = handles.recording.lock().as_mut() {
            recording_output::append_now(recording, chunk);
            true
        } else {
            false
        };
        handles.activity.lock().mark_output(
            chunk.len(),
            terminal_response_bytes,
            recorded,
            started_at.elapsed(),
        );

        OutputApplyStats {
            input_bytes: chunk.len(),
            terminal_response_bytes,
            recorded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    struct SharedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn apply_chunk_updates_output_screen_activity_and_queries() {
        let output = Arc::new(Mutex::new(String::new()));
        let screen = Arc::new(Mutex::new(NextCoreScreen::new(80, 24)));
        let recording = Arc::new(Mutex::new(None));
        let activity = Arc::new(Mutex::new(SessionIoActivity::new()));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));
        let mut pending = String::new();

        let stats = apply_chunk(
            OutputHandles {
                output: &output,
                screen: &screen,
                recording: &recording,
                activity: &activity,
                writer: &writer,
            },
            "hello\x1b[6n",
            &mut pending,
        );

        assert_eq!(output.lock().as_str(), "hello\x1b[6n");
        assert_eq!(screen.lock().snapshot_viewport_lines()[0], "hello");
        assert_eq!(bytes.lock().as_slice(), b"\x1b[1;6R");
        assert_eq!(
            activity.lock().output.as_ref().unwrap().total_bytes,
            "hello\x1b[6n".len() as u64
        );
        assert_eq!(
            stats,
            OutputApplyStats {
                input_bytes: "hello\x1b[6n".len(),
                terminal_response_bytes: 6,
                recorded: false,
            }
        );
    }

}
