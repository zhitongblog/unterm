use super::super::{health_snapshot as health_snapshot_engine, session_activity, session_queries};
use super::{command::RuntimeCommand, with_current_mut};
use crate::{EngineHealthSnapshot, SessionActivitySnapshot, ShellSnapshot};
use anyhow::{bail, Result};

pub(in crate::next_core) fn execute_output(command: RuntimeCommand) -> Result<String> {
    match command {
        RuntimeCommand::RawOutput { pane_id } => session_queries::output(pane_id),
        _ => bail!("runtime status executor expected raw-output command"),
    }
}

pub(in crate::next_core) fn execute_shell_snapshot(
    command: RuntimeCommand,
) -> Result<ShellSnapshot> {
    match command {
        RuntimeCommand::ShellSnapshot { pane_id } => session_queries::shell_snapshot(pane_id),
        _ => bail!("runtime status executor expected shell-snapshot command"),
    }
}

pub(in crate::next_core) fn execute_session_activity(
    command: RuntimeCommand,
) -> Result<SessionActivitySnapshot> {
    match command {
        RuntimeCommand::SessionActivity { pane_id } => {
            session_activity::read_snapshot_concurrent(pane_id, std::time::Instant::now())
        }
        _ => bail!("runtime status executor expected session-activity command"),
    }
}

pub(in crate::next_core) fn execute_health_snapshot(
    command: RuntimeCommand,
) -> Result<EngineHealthSnapshot> {
    match command {
        RuntimeCommand::HealthSnapshot => Ok(with_current_mut(health_snapshot_engine::snapshot)),
        _ => bail!("runtime status executor expected health-snapshot command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_executor_rejects_wrong_command_shape() {
        let err = execute_shell_snapshot(RuntimeCommand::HealthSnapshot)
            .expect_err("wrong status command should fail");

        assert!(err.to_string().contains("expected shell-snapshot command"));

        let err = execute_health_snapshot(RuntimeCommand::RawOutput { pane_id: 1 })
            .expect_err("wrong health command should fail");

        assert!(err.to_string().contains("expected health-snapshot command"));
    }

    #[test]
    fn health_snapshot_status_command_is_infallible() {
        let health =
            execute_health_snapshot(RuntimeCommand::HealthSnapshot).expect("health snapshot");

        assert_eq!(health.engine, "next-core");
    }
}
