//! Reserved adapter for Apple's Virtualization.framework.
//! The initial runnable backend is QEMU with HVF on matching macOS hosts.
//! Never silently substitute emulation for a requested native Apple VM.
use std::path::PathBuf;
use vm_core::{VmBackend, VmConfig, VmError, VmState};

pub struct AppleBackend;
impl VmBackend for AppleBackend {
    fn start(&self, _: &VmConfig) -> Result<(), VmError> {
        Err(VmError::Backend(
            "Apple Virtualization.framework adapter is not implemented; select Qemu".into(),
        ))
    }
    fn stop(&self, _: &str) -> Result<(), VmError> {
        Err(VmError::Backend("Apple backend is not implemented".into()))
    }
    fn state(&self, _: &str) -> Result<VmState, VmError> {
        Ok(VmState::Stopped)
    }
    fn console_path(&self, _: &str) -> Result<PathBuf, VmError> {
        Err(VmError::Backend("Apple backend is not implemented".into()))
    }
}
