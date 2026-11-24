use std::{
    fs::{self, OpenOptions},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use vm_core::VmError;

pub(crate) struct Tpm(Child);

impl Tpm {
    pub(crate) fn start(
        binary: &Path,
        socket: &Path,
        state: &Path,
        log: &Path,
    ) -> Result<Self, VmError> {
        let option_path = |path: &Path| {
            path.to_str()
                .filter(|path| !path.contains(','))
                .map(str::to_owned)
                .ok_or_else(|| {
                    VmError::InvalidConfig(
                        "TPM paths must be UTF-8 and cannot contain commas".into(),
                    )
                })
        };
        let socket_option = option_path(socket)?;
        let state_option = option_path(state)?;
        match fs::symlink_metadata(state) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(VmError::InvalidConfig(
                    "TPM state must be a regular file".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match fs::symlink_metadata(socket) {
            Ok(metadata) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if !metadata.file_type().is_socket() {
                        return Err(VmError::InvalidConfig(
                            "TPM control path already exists and is not a socket".into(),
                        ));
                    }
                    fs::remove_file(socket)?;
                }
                #[cfg(not(unix))]
                return Err(VmError::Backend("TPM requires Unix sockets".into()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let errors = OpenOptions::new().append(true).create(true).open(log)?;
        let mut tpm = Self(
            Command::new(binary)
                .args([
                    "socket",
                    "--tpm2",
                    "--tpmstate",
                    &format!("backend-uri=file://{state_option},mode=0600,lock"),
                    "--ctrl",
                    &format!("type=unixio,path={socket_option},mode=0600"),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(errors)
                .spawn()
                .map_err(|error| {
                    VmError::Backend(format!(
                        "Could not launch swtpm. Install it with brew install swtpm. {error}"
                    ))
                })?,
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if tpm.0.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(VmError::Backend(format!(
                    "The virtual TPM did not start. See {}",
                    log.display()
                )));
            }
            if socket.exists() {
                return Ok(tpm);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Tpm {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
