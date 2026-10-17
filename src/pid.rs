use std::fs;
use std::io::Write;
use std::process;

pub struct PidFile {
    path: String,
}

impl PidFile {
    pub fn create(path: &str) -> anyhow::Result<Self> {
        if let Some(pid) = fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
                anyhow::bail!("daemon already running with PID {pid} (PID file: {path})");
            }
            tracing::warn!("removing stale PID file for PID {pid}");
        }

        let mut file = fs::File::create(path)?;
        write!(file, "{}", process::id())?;
        Ok(PidFile {
            path: path.to_string(),
        })
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            tracing::error!("failed to remove PID file {}: {e}", self.path);
        }
    }
}
