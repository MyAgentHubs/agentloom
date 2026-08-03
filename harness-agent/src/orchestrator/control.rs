use std::path::{Path, PathBuf};

use serde_json::json;
use uuid::Uuid;

use super::ControlInputKind;
use crate::control::{
    ControlCommand, ControlSource, SentinelFileControlSource, StdinJsonlControlSource,
};
use crate::error::Result;
use crate::events::EventRecorder;
use crate::journal::RunPaths;

pub(crate) fn make_control_source(
    control_input: ControlInputKind,
    paths: &RunPaths,
    run_id: &str,
) -> Box<dyn ControlSource> {
    match control_input {
        ControlInputKind::StdinJsonl => Box::new(StdinJsonlControlSource::new()),
        ControlInputKind::Sentinel => Box::new(SentinelFileControlSource::new(
            paths.interrupt_path.clone(),
            run_id,
        )),
    }
}

pub(crate) fn handle_control(
    control: &mut dyn ControlSource,
    recorder: &mut EventRecorder,
    run_id: &str,
    step_id: &str,
) -> Result<bool> {
    if matches!(
        control.poll(),
        Some(ControlCommand::Stop { .. } | ControlCommand::Pause { .. })
    ) {
        recorder.emit(
            "run.interrupted",
            json!({
                "step_id": step_id,
                "resume_command": format!("myagent resume {run_id}"),
            }),
        )?;
        return Ok(true);
    }
    Ok(false)
}

pub fn request_interrupt(journal_root: impl AsRef<Path>, run_id: &str) -> Result<PathBuf> {
    let paths = RunPaths::new(journal_root.as_ref(), run_id);
    std::fs::create_dir_all(&paths.run_dir)?;
    std::fs::write(&paths.interrupt_path, b"interrupt\n")?;
    Ok(paths.interrupt_path)
}

pub fn new_run_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("run_{}", &uuid[..8])
}
