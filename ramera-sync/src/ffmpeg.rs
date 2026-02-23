use std::path::Path;
use std::process::Command;

use crate::error::{AppError, Result};

pub fn install_local_ffmpeg(dir: &Path, force: bool) -> Result<()> {
    let script = Path::new("scripts").join("install_ffmpeg.sh");
    if !script.exists() {
        return Err(AppError::Command(format!(
            "missing installer script: {}",
            script.display()
        )));
    }

    let mut cmd = Command::new("bash");
    cmd.arg(&script).arg(dir);
    if force {
        cmd.arg("--force");
    }

    let status = cmd.status()?;
    if !status.success() {
        return Err(AppError::Command(format!(
            "ffmpeg installer failed with status {status}"
        )));
    }

    Ok(())
}
