//! Backend-independent VM configuration and lifecycle contract.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuestArchitecture {
    Aarch64,
    X86_64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Qemu,
    Apple,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Stopped,
    Running,
    Failed,
}

/// User-facing network modes. NAT and disconnected are implemented by the MVP.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    Nat,
    NatNetwork,
    Bridged,
    HostOnly,
    Internal,
    Disconnected,
}

/// Select the installer explicitly, or boot the persistent system disk.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BootMode {
    #[default]
    Auto,
    Installer,
    Disk,
}

/// Mutable machine settings. Storage identity and guest architecture are immutable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmSettingsUpdate {
    pub name: String,
    pub cpus: u8,
    pub memory_mib: u64,
    pub network_mode: NetworkMode,
    pub iso_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmConfig {
    pub id: String,
    pub name: String,
    pub cpus: u8,
    pub memory_mib: u64,
    pub disk_path: PathBuf,
    pub disk_size_gib: u64,
    #[serde(default)]
    pub boot_mode: BootMode,
    #[serde(default)]
    pub firmware_code_path: Option<PathBuf>,
    #[serde(default)]
    pub firmware_vars_template_path: Option<PathBuf>,
    /// Optional direct Linux kernel boot; firmware boot is used when empty.
    #[serde(default)]
    pub kernel_path: PathBuf,
    /// Read-only installation media attached only in installer mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_path: Option<PathBuf>,
    pub initrd_path: Option<PathBuf>,
    pub kernel_cmdline: String,
    pub architecture: GuestArchitecture,
    pub backend: BackendKind,
    #[serde(default)]
    pub network_mode: NetworkMode,
}
impl VmConfig {
    pub fn direct_kernel_boot(&self) -> bool {
        self.boot_mode == BootMode::Auto && !self.kernel_path.as_os_str().is_empty()
    }
    pub fn installer_boot(&self) -> bool {
        self.boot_mode == BootMode::Installer
            || (self.boot_mode == BootMode::Auto
                && !self.direct_kernel_boot()
                && self.iso_path.is_some())
    }
    pub fn validate(&self) -> Result<(), VmError> {
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(VmError::InvalidConfig("name must not be empty".into()));
        }
        if self.cpus == 0 || self.cpus > 64 {
            return Err(VmError::InvalidConfig("cpus must be 1..=64".into()));
        }
        if !(128..=1_048_576).contains(&self.memory_mib) {
            return Err(VmError::InvalidConfig(
                "memory_mib must be 128..=1048576".into(),
            ));
        }
        if !(1..=16384).contains(&self.disk_size_gib) {
            return Err(VmError::InvalidConfig(
                "disk_size_gib must be 1..=16384".into(),
            ));
        }
        if self.direct_kernel_boot() {
            for (label, path) in [
                ("kernel", Some(&self.kernel_path)),
                ("initrd", self.initrd_path.as_ref()),
            ] {
                if let Some(path) = path {
                    if !path.is_absolute() || !path.is_file() {
                        return Err(VmError::InvalidConfig(format!(
                            "{label} must be an existing absolute file path"
                        )));
                    }
                }
            }
        }
        if self.installer_boot() {
            let iso = self.iso_path.as_ref().ok_or_else(|| {
                VmError::InvalidConfig("Installer boot requires an ISO image".into())
            })?;
            if !iso.is_absolute()
                || !iso.is_file()
                || !iso
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
            {
                return Err(VmError::InvalidConfig(
                    "ISO image must be an existing absolute .iso file path".into(),
                ));
            }
        }
        for path in [&self.firmware_code_path, &self.firmware_vars_template_path]
            .into_iter()
            .flatten()
        {
            if !path.is_absolute() || !path.is_file() {
                return Err(VmError::InvalidConfig(
                    "Firmware must be an existing absolute file path".into(),
                ));
            }
        }
        if !self.disk_path.is_absolute() {
            return Err(VmError::InvalidConfig("disk_path must be absolute".into()));
        }
        if self.kernel_cmdline.contains('\0') {
            return Err(VmError::InvalidConfig(
                "kernel command line contains NUL".into(),
            ));
        }
        Ok(())
    }
}
pub fn validate_id(id: &str) -> Result<(), VmError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(VmError::InvalidConfig(
            "id must contain 1..=64 ASCII letters, digits, '-' or '_'".into(),
        ));
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInfo {
    pub config: VmConfig,
    pub state: VmState,
}
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("VM not found: {0}")]
    NotFound(String),
    #[error("VM already running: {0}")]
    AlreadyRunning(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub trait VmBackend: Send + Sync {
    fn start(&self, config: &VmConfig) -> Result<(), VmError>;
    fn stop(&self, id: &str) -> Result<(), VmError>;
    /// Request guest shutdown without forcibly terminating it.
    fn shutdown(&self, _id: &str) -> Result<(), VmError> {
        Err(VmError::Backend(
            "graceful shutdown is unsupported by this backend".into(),
        ))
    }
    /// Explicit immediate termination, including cleanup of failed instances.
    fn force_stop(&self, _id: &str) -> Result<(), VmError> {
        Err(VmError::Backend(
            "force stop is unsupported by this backend".into(),
        ))
    }
    fn state(&self, id: &str) -> Result<VmState, VmError>;
    fn console_path(&self, id: &str) -> Result<PathBuf, VmError>;
}
