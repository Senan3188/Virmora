//! Persistent VM registry. Existing raw disks can be imported and are never truncated.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use vm_core::{
    validate_id, BootMode, VmBackend, VmConfig, VmError, VmInfo, VmSettingsUpdate, VmState,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

pub struct VmManager {
    root: PathBuf,
    backend: Arc<dyn VmBackend>,
    mutation: Mutex<()>,
    _registry_lock: File,
}
impl VmManager {
    pub fn new(root: impl AsRef<Path>, backend: Arc<dyn VmBackend>) -> Result<Self, VmError> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        let registry_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".registry.lock"))?;
        registry_lock.try_lock().map_err(|e| {
            VmError::Backend(format!(
                "registry is already in use or cannot be locked: {e}"
            ))
        })?;
        Ok(Self {
            root,
            backend,
            mutation: Mutex::new(()),
            _registry_lock: registry_lock,
        })
    }
    fn config_path(&self, id: &str) -> Result<PathBuf, VmError> {
        validate_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
    pub fn get(&self, id: &str) -> Result<VmInfo, VmError> {
        let path = self.config_path(id)?;
        let bytes = fs::read(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                VmError::NotFound(id.into())
            } else {
                e.into()
            }
        })?;
        let config: VmConfig = serde_json::from_slice(&bytes)?;
        if config.id != id {
            return Err(VmError::InvalidConfig("registry ID mismatch".into()));
        }
        Ok(VmInfo {
            state: self.backend.state(id)?,
            config,
        })
    }
    pub fn list(&self) -> Result<Vec<VmInfo>, VmError> {
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().is_some_and(|v| v == "json") {
                if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                    result.push(self.get(id)?);
                }
            }
        }
        result.sort_by(|a, b| a.config.id.cmp(&b.config.id));
        Ok(result)
    }
    pub fn create(&self, mut config: VmConfig) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        config.name = config.name.trim().to_owned();
        config.validate()?;
        let normalized_name = config.name.to_lowercase();
        if self
            .list()?
            .iter()
            .any(|vm| vm.config.name.trim().to_lowercase() == normalized_name)
        {
            return Err(VmError::InvalidConfig(format!(
                "A machine named '{}' already exists. Choose a different name.",
                config.name
            )));
        }
        let path = self.config_path(&config.id)?;
        if path.exists() {
            return Err(VmError::InvalidConfig(format!(
                "VM {} already exists",
                config.id
            )));
        }
        let disk_created = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.disk_path)
        {
            Ok(disk) => {
                if let Err(e) = disk
                    .set_len(config.disk_size_gib * 1024 * 1024 * 1024)
                    .and_then(|_| disk.sync_all())
                {
                    let _ = fs::remove_file(&config.disk_path);
                    return Err(e.into());
                }
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&config.disk_path)?;
                if !metadata.file_type().is_file()
                    || metadata.len() != config.disk_size_gib * 1024 * 1024 * 1024
                {
                    return Err(VmError::InvalidConfig("existing disk must be a regular, non-symlink raw file matching disk_size_gib".into()));
                }
                let canonical = fs::canonicalize(&config.disk_path)?;
                for vm in self.list()? {
                    if fs::canonicalize(&vm.config.disk_path).is_ok_and(|p| p == canonical) {
                        return Err(VmError::InvalidConfig(
                            "disk is already attached to another VM".into(),
                        ));
                    }
                    // Hard links can have different canonical paths but refer to the same file.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if let Ok(other) = fs::metadata(&vm.config.disk_path) {
                            if other.dev() == metadata.dev() && other.ino() == metadata.ino() {
                                return Err(VmError::InvalidConfig(
                                    "disk is already attached to another VM".into(),
                                ));
                            }
                        }
                    }
                }
                false
            }
            Err(e) => return Err(e.into()),
        };
        let temporary = self.root.join(format!(".{}.json.tmp", config.id));
        let write_result = (|| -> Result<(), VmError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&config)?)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = fs::remove_file(&temporary);
            if disk_created && !path.exists() {
                let _ = fs::remove_file(&config.disk_path);
            }
            return Err(e);
        }
        self.get(&config.id)
    }
    /// Change the next boot source without modifying the persistent guest disk.
    pub fn update_boot_mode(&self, id: &str, mode: BootMode) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        let mut vm = self.get(id)?;
        if vm.state != VmState::Stopped {
            return Err(VmError::InvalidConfig(
                "stop the VM before changing its boot mode".into(),
            ));
        }
        vm.config.boot_mode = mode;
        vm.config.validate()?;
        let path = self.config_path(id)?;
        let temporary = self.root.join(format!(".{id}.json.tmp"));
        let result = (|| -> Result<(), VmError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&vm.config)?)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result?;
        self.get(id)
    }
    fn save_config(&self, config: &VmConfig) -> Result<(), VmError> {
        write_atomic(
            &self.config_path(&config.id)?,
            &serde_json::to_vec_pretty(config)?,
        )
    }
    pub fn update_settings(&self, id: &str, update: VmSettingsUpdate) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        let mut vm = self.get(id)?;
        if vm.state == VmState::Running {
            return Err(VmError::InvalidConfig(
                "stop the VM before editing its settings".into(),
            ));
        }
        vm.config.name = update.name.trim().to_owned();
        vm.config.cpus = update.cpus;
        vm.config.memory_mib = update.memory_mib;
        vm.config.network_mode = update.network_mode;
        vm.config.bridge_interface = update.bridge_interface;
        vm.config.iso_path = update.iso_path;
        vm.config.drivers_iso_path = update.drivers_iso_path;
        if vm.config.iso_path.is_none() && vm.config.boot_mode == BootMode::Installer {
            vm.config.boot_mode = BootMode::Disk;
        }
        // Explicitly validate replacement media even when the disk is the active boot source.
        if let Some(iso) = &vm.config.iso_path {
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
        vm.config.validate()?;
        let name = vm.config.name.to_lowercase();
        if self
            .list()?
            .iter()
            .any(|other| other.config.id != id && other.config.name.trim().to_lowercase() == name)
        {
            return Err(VmError::InvalidConfig(format!(
                "A machine named '{}' already exists. Choose a different name.",
                vm.config.name
            )));
        }
        self.save_config(&vm.config)?;
        self.get(id)
    }
    fn error_path(&self, id: &str) -> Result<PathBuf, VmError> {
        validate_id(id)?;
        Ok(self.root.join(format!(".{id}.startup-error.txt")))
    }
    pub fn last_error(&self, id: &str) -> Result<Option<String>, VmError> {
        self.get(id)?;
        match fs::read_to_string(self.error_path(id)?) {
            Ok(error) => Ok(Some(error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn start(&self, id: &str) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        self.start_locked(id)
    }
    fn start_locked(&self, id: &str) -> Result<VmInfo, VmError> {
        let vm = self.get(id)?;
        let result = (|| -> Result<(), VmError> {
            vm.config.validate()?;
            if !vm.config.disk_path.is_file() {
                return Err(VmError::InvalidConfig("persistent disk is missing".into()));
            }
            self.backend.start(&vm.config)
        })();
        if let Err(error) = result {
            if let Err(storage_error) =
                write_atomic(&self.error_path(id)?, error.to_string().as_bytes())
            {
                return Err(VmError::Backend(format!(
                    "{error}; could not save startup diagnostics: {storage_error}"
                )));
            }
            return Err(error);
        }
        match fs::remove_file(self.error_path(id)?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("could not clear startup diagnostics for {id}: {error}"),
        }
        self.get(id)
    }
    pub fn shutdown(&self, id: &str) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        self.get(id)?;
        self.backend.shutdown(id)?;
        self.get(id)
    }
    pub fn force_stop(&self, id: &str) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        self.get(id)?;
        self.backend.force_stop(id)?;
        self.get(id)
    }
    pub fn restart(&self, id: &str) -> Result<VmInfo, VmError> {
        self.restart_with_timeout(id, Duration::from_secs(30))
    }
    fn restart_with_timeout(&self, id: &str, timeout: Duration) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        if self.get(id)?.state != VmState::Running {
            return Err(VmError::InvalidConfig(
                "restart requires a running VM".into(),
            ));
        }
        self.backend.shutdown(id)?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.get(id)?.state {
                VmState::Stopped => return self.start_locked(id),
                VmState::Failed => {
                    return Err(VmError::Backend(
                        "guest failed during shutdown; use Force Stop before starting again".into(),
                    ))
                }
                VmState::Running => {}
            }
            if Instant::now() >= deadline {
                return Err(VmError::Backend("guest did not shut down within 30 seconds; restart cancelled without forcing termination".into()));
            }
            thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    pub fn stop(&self, id: &str) -> Result<VmInfo, VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        self.get(id)?;
        self.backend.stop(id)?;
        self.get(id)
    }
    /// Remove a stopped machine. Retained disks receive an importable recovery configuration.
    pub fn delete(&self, id: &str, keep_disk: bool) -> Result<(), VmError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| VmError::Backend("manager lock poisoned".into()))?;
        let vm = self.get(id)?;
        if vm.state != VmState::Stopped {
            return Err(VmError::InvalidConfig(
                "stop the VM before deleting it".into(),
            ));
        }
        let registry = self.config_path(id)?;
        let nvram = append_path(&vm.config.disk_path, ".nvram.fd");
        if keep_disk {
            let mut recovery = append_path(&vm.config.disk_path, ".virmora-recovery");
            let mut suffix = 1;
            while fs::symlink_metadata(&recovery).is_ok() {
                recovery =
                    append_path(&vm.config.disk_path, &format!(".virmora-recovery-{suffix}"));
                suffix += 1;
            }
            // create_new refuses to overwrite an unrelated recovery file or a symlink.
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&recovery)?;
            let result = (|| -> Result<(), VmError> {
                output.write_all(&serde_json::to_vec_pretty(&vm.config)?)?;
                output.sync_all()?;
                File::open(
                    recovery
                        .parent()
                        .ok_or_else(|| VmError::InvalidConfig("disk has no parent".into()))?,
                )?
                .sync_all()?;
                fs::remove_file(&registry)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(recovery);
            }
            result?;
        } else {
            let others = self.list()?;
            let mut targets = Vec::new();
            let tpm = vm.config.tpm_state_path();
            let mut files = vec![&vm.config.disk_path, &nvram];
            if vm.config.guest_os == vm_core::GuestOs::Windows11 {
                files.push(&tpm);
            }
            for target in files {
                match fs::symlink_metadata(target) {
                    Ok(metadata) => {
                        if !metadata.file_type().is_file() {
                            return Err(VmError::InvalidConfig(
                                "refusing to delete a VM file that is a symlink or non-file".into(),
                            ));
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            if metadata.nlink() != 1 {
                                return Err(VmError::InvalidConfig(
                                    "refusing to delete a hard-linked VM file".into(),
                                ));
                            }
                        }
                        for other in &others {
                            let other_nvram = append_path(&other.config.disk_path, ".nvram.fd");
                            let protected = [
                                Some(&other.config.kernel_path),
                                other.config.initrd_path.as_ref(),
                                other.config.iso_path.as_ref(),
                                other.config.drivers_iso_path.as_ref(),
                                other.config.firmware_code_path.as_ref(),
                                other.config.firmware_vars_template_path.as_ref(),
                            ];
                            for asset in protected.into_iter().flatten() {
                                if same_file(target, asset) {
                                    return Err(VmError::InvalidConfig("refusing to delete a VM file that aliases installation or boot media".into()));
                                }
                            }
                            if other.config.id != id
                                && (same_file(target, &other.config.disk_path)
                                    || same_file(target, &other_nvram)
                                    || same_file(target, &other.config.tpm_state_path()))
                            {
                                return Err(VmError::InvalidConfig(
                                    "refusing to delete a file attached to another VM".into(),
                                ));
                            }
                        }
                        targets.push(target.clone());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            // Stage only the two named VM files, beside their originals (works across volumes).
            // All fallible staging occurs before removing the registry; rollback retains registration.
            let nonce = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
            let staging = (|| -> Result<(), VmError> {
                for original in &targets {
                    let hidden = append_path(
                        original,
                        &format!(".virmora-delete-{}-{nonce}", std::process::id()),
                    );
                    if hidden.try_exists()? {
                        return Err(VmError::InvalidConfig(
                            "deletion staging path already exists".into(),
                        ));
                    }
                    fs::rename(original, &hidden)?;
                    staged.push((original.clone(), hidden));
                }
                fs::remove_file(&registry)?;
                Ok(())
            })();
            if let Err(error) = staging {
                let mut rollback_errors = Vec::new();
                for (original, hidden) in staged.iter().rev() {
                    if let Err(rollback) = fs::rename(hidden, original) {
                        rollback_errors.push(format!("{}: {rollback}", hidden.display()));
                    }
                }
                if !rollback_errors.is_empty() {
                    return Err(VmError::Backend(format!(
                        "{error}; restore staged files manually: {}",
                        rollback_errors.join(", ")
                    )));
                }
                return Err(error);
            }
            // Registration is committed. Keep recovery metadata if final unlink fails.
            for (_, hidden) in &staged {
                if let Err(error) = fs::remove_file(hidden) {
                    let receipt = self.root.join(format!(".{id}.deletion-recovery.txt"));
                    let remaining: Vec<_> = staged
                        .iter()
                        .filter(|(_, path)| path.exists())
                        .map(|(original, path)| {
                            format!("{} -> {}", path.display(), original.display())
                        })
                        .collect();
                    let message = format!(
                        "{}\nRemaining files: {}\nConfiguration: {}",
                        error,
                        remaining.join(", "),
                        serde_json::to_string_pretty(&vm.config)?
                    );
                    write_atomic(&receipt, message.as_bytes())?;
                    return Err(VmError::Backend(format!("machine registration removed; file cleanup failed: {error}; recovery details: {}", receipt.display())));
                }
            }
        }
        let _ = fs::remove_file(self.error_path(id)?);
        Ok(())
    }
    pub fn console_path(&self, id: &str) -> Result<PathBuf, VmError> {
        self.get(id)?;
        self.backend.console_path(id)
    }
}

fn append_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(left), fs::metadata(right)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Replace metadata atomically. Temporary files are created exclusively to avoid symlink reuse.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), VmError> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicU64, Ordering},
    };
    use vm_core::{BackendKind, GuestArchitecture};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "virmora-manager-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn config(&self) -> VmConfig {
            let kernel = self.0.join("kernel");
            fs::write(&kernel, b"test-kernel").unwrap();
            VmConfig {
                guest_os: vm_core::GuestOs::Linux,
                drivers_iso_path: None,
                id: "linux".into(),
                name: "Linux".into(),
                cpus: 2,
                memory_mib: 512,
                disk_path: self.0.join("disk.raw"),
                disk_size_gib: 1,
                kernel_path: kernel,
                initrd_path: None,
                kernel_cmdline: "console=ttyAMA0".into(),
                architecture: GuestArchitecture::Aarch64,
                iso_path: None,
                boot_mode: BootMode::Auto,
                firmware_code_path: None,
                firmware_vars_template_path: None,
                backend: BackendKind::Qemu,
                network_mode: vm_core::NetworkMode::Nat,
                bridge_interface: None,
            }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Default)]
    struct Mock(Mutex<HashSet<String>>, std::sync::atomic::AtomicBool);
    impl VmBackend for Mock {
        fn start(&self, c: &VmConfig) -> Result<(), VmError> {
            if !self.0.lock().unwrap().insert(c.id.clone()) {
                return Err(VmError::AlreadyRunning(c.id.clone()));
            }
            Ok(())
        }
        fn stop(&self, id: &str) -> Result<(), VmError> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
        fn shutdown(&self, id: &str) -> Result<(), VmError> {
            if self.1.load(Ordering::Relaxed) {
                return self.stop(id);
            }
            Ok(())
        }
        fn force_stop(&self, id: &str) -> Result<(), VmError> {
            self.stop(id)
        }
        fn state(&self, id: &str) -> Result<VmState, VmError> {
            Ok(if self.0.lock().unwrap().contains(id) {
                VmState::Running
            } else {
                VmState::Stopped
            })
        }
        fn console_path(&self, id: &str) -> Result<PathBuf, VmError> {
            Ok(PathBuf::from(format!("/tmp/{id}.sock")))
        }
    }
    #[test]
    fn settings_eject_iso_without_replacing_disk_or_allowing_running_edits() {
        let temp = Temp::new();
        let mut config = temp.config();
        let iso = temp.0.join("installer.iso");
        fs::write(&iso, b"installer").unwrap();
        config.iso_path = Some(iso.clone());
        config.boot_mode = BootMode::Installer;
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        File::options()
            .write(true)
            .open(&config.disk_path)
            .unwrap()
            .write_all(b"marker")
            .unwrap();
        let update = || VmSettingsUpdate {
            name: "  Renamed  ".into(),
            cpus: 3,
            memory_mib: 768,
            network_mode: vm_core::NetworkMode::Disconnected,
            bridge_interface: None,
            iso_path: None,
            drivers_iso_path: None,
        };
        manager.start("linux").unwrap();
        assert!(manager.update_settings("linux", update()).is_err());
        manager.force_stop("linux").unwrap();
        let saved = manager.update_settings("linux", update()).unwrap().config;
        assert_eq!(saved.name, "Renamed");
        assert_eq!((saved.cpus, saved.memory_mib), (3, 768));
        assert_eq!(saved.network_mode, vm_core::NetworkMode::Disconnected);
        assert_eq!(saved.boot_mode, BootMode::Disk);
        assert_eq!(saved.iso_path, None);
        assert_eq!(saved.disk_path, config.disk_path);
        let mut prefix = [0; 6];
        std::io::Read::read_exact(&mut File::open(saved.disk_path).unwrap(), &mut prefix).unwrap();
        assert_eq!(&prefix, b"marker");
        assert!(iso.is_file());
    }

    #[test]
    fn restart_timeout_leaves_guest_running_and_deletion_requires_stop() {
        let temp = Temp::new();
        let config = temp.config();
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        manager.start("linux").unwrap();
        assert_eq!(manager.shutdown("linux").unwrap().state, VmState::Running);
        assert!(manager
            .restart_with_timeout("linux", Duration::from_millis(10))
            .is_err());
        assert_eq!(manager.get("linux").unwrap().state, VmState::Running);
        assert!(manager.delete("linux", false).is_err());
        assert!(config.disk_path.is_file());
        assert_eq!(manager.force_stop("linux").unwrap().state, VmState::Stopped);
    }

    #[test]
    fn restart_starts_again_after_guest_exits() {
        let temp = Temp::new();
        let backend = Arc::new(Mock::default());
        backend.1.store(true, Ordering::Relaxed);
        let manager = VmManager::new(temp.0.join("registry"), backend).unwrap();
        manager.create(temp.config()).unwrap();
        manager.start("linux").unwrap();
        assert_eq!(manager.restart("linux").unwrap().state, VmState::Running);
    }

    #[test]
    fn deletion_preserves_media_and_can_restore_a_retained_disk() {
        let temp = Temp::new();
        let mut config = temp.config();
        let iso = temp.0.join("installer.iso");
        fs::write(&iso, b"installer").unwrap();
        config.iso_path = Some(iso.clone());
        let nvram = append_path(&config.disk_path, ".nvram.fd");
        let manager = VmManager::new(&temp.0, Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        fs::write(&nvram, b"firmware settings").unwrap();
        manager.delete("linux", true).unwrap();
        assert!(manager.list().unwrap().is_empty());
        assert!(config.disk_path.is_file());
        assert!(nvram.is_file());
        let recovery = append_path(&config.disk_path, ".virmora-recovery");
        let restored = serde_json::from_slice(&fs::read(recovery).unwrap()).unwrap();
        manager.create(restored).unwrap();
        manager.delete("linux", false).unwrap();
        assert!(manager.list().unwrap().is_empty());
        assert!(!config.disk_path.exists());
        assert!(!nvram.exists());
        assert!(iso.is_file());
        assert!(config.kernel_path.is_file());
    }

    #[test]
    fn windows_driver_eject_and_disk_retention_preserve_tpm_state() {
        let temp = Temp::new();
        let mut config = temp.config();
        config.guest_os = vm_core::GuestOs::Windows11;
        config.architecture = vm_core::GuestArchitecture::Aarch64;
        config.kernel_path.clear();
        config.memory_mib = 4096;
        config.disk_size_gib = 64;
        config.boot_mode = BootMode::Disk;
        let drivers = temp.0.join("drivers.iso");
        fs::write(&drivers, b"drivers").unwrap();
        config.drivers_iso_path = Some(drivers.clone());
        let manager = VmManager::new(&temp.0, Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        let tpm = config.tpm_state_path();
        fs::write(&tpm, b"TPM state").unwrap();

        let update = VmSettingsUpdate {
            name: config.name,
            cpus: config.cpus,
            memory_mib: config.memory_mib,
            network_mode: config.network_mode,
            bridge_interface: None,
            iso_path: None,
            drivers_iso_path: None,
        };
        let edited = manager.update_settings(&config.id, update).unwrap();
        assert_eq!(edited.config.drivers_iso_path, None);
        assert_eq!(edited.config.boot_mode, BootMode::Disk);
        manager.delete(&config.id, true).unwrap();
        assert_eq!(fs::read(&tpm).unwrap(), b"TPM state");

        let recovery = append_path(&config.disk_path, ".virmora-recovery");
        let restored = serde_json::from_slice(&fs::read(recovery).unwrap()).unwrap();
        manager.create(restored).unwrap();
        manager.delete(&config.id, false).unwrap();
        assert!(!tpm.exists());
        assert_eq!(fs::read(drivers).unwrap(), b"drivers");
    }

    #[test]
    fn startup_error_survives_reopen_and_clears_after_start() {
        let temp = Temp::new();
        let config = temp.config();
        let registry = temp.0.join("registry");
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        let moved = temp.0.join("moved.raw");
        fs::rename(&config.disk_path, &moved).unwrap();
        assert!(manager.start("linux").is_err());
        drop(manager);
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        assert!(manager
            .last_error("linux")
            .unwrap()
            .unwrap()
            .contains("disk is missing"));
        fs::rename(moved, &config.disk_path).unwrap();
        manager.start("linux").unwrap();
        assert_eq!(manager.last_error("linux").unwrap(), None);
    }

    #[test]
    fn duplicate_names_are_rejected_after_registry_reopen_without_creating_disk() {
        let temp = Temp::new();
        let mut first = temp.config();
        first.name = "  linux machine  ".into();
        let registry = temp.0.join("registry");
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        assert_eq!(
            manager.create(first.clone()).unwrap().config.name,
            "linux machine"
        );
        drop(manager);
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        for name in ["linux machine", "LINUX MACHINE", "  Linux Machine  "] {
            let mut duplicate = first.clone();
            duplicate.id = "second".into();
            duplicate.name = name.into();
            duplicate.disk_path = temp.0.join("second.raw");
            let error = manager.create(duplicate.clone()).unwrap_err().to_string();
            assert!(error.contains("already exists"));
            assert!(!duplicate.disk_path.exists());
            assert!(!registry.join("second.json").exists());
        }
        let mut distinct = first;
        distinct.id = "second".into();
        distinct.name = "linux machine 2".into();
        distinct.disk_path = temp.0.join("second.raw");
        manager.create(distinct).unwrap();
        assert_eq!(manager.list().unwrap().len(), 2);
    }
    #[test]
    fn iso_only_configuration_is_saved_and_reopened_without_a_kernel() {
        let temp = Temp::new();
        let mut config = temp.config();
        let iso = temp.0.join("installer.iso");
        fs::write(&iso, b"test installer reference").unwrap();
        config.kernel_path = PathBuf::new();
        config.iso_path = Some(iso.clone());
        config.network_mode = vm_core::NetworkMode::HostOnly;
        let registry = temp.0.join("registry");
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        manager.create(config).unwrap();
        drop(manager);
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        let saved = manager.get("linux").unwrap();
        assert_eq!(saved.config.iso_path, Some(iso));
        assert_eq!(manager.start("linux").unwrap().state, VmState::Running);
        manager.stop("linux").unwrap();
        assert_eq!(saved.config.network_mode, vm_core::NetworkMode::HostOnly);
        assert!(saved.config.kernel_path.as_os_str().is_empty());
    }
    #[test]
    fn registry_survives_reopen_and_disk_contents_survive_lifecycle() {
        let temp = Temp::new();
        let config = temp.config();
        let registry = temp.0.join("registry");
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        assert_eq!(
            manager.create(config.clone()).unwrap().state,
            VmState::Stopped
        );
        assert_eq!(
            fs::metadata(&config.disk_path).unwrap().len(),
            1024 * 1024 * 1024
        );
        let mut disk = OpenOptions::new()
            .write(true)
            .open(&config.disk_path)
            .unwrap();
        disk.write_all(b"persistent").unwrap();
        assert_eq!(manager.start("linux").unwrap().state, VmState::Running);
        assert!(matches!(
            manager.start("linux"),
            Err(VmError::AlreadyRunning(_))
        ));
        assert_eq!(manager.stop("linux").unwrap().state, VmState::Stopped);
        drop(manager);
        let manager = VmManager::new(registry, Arc::new(Mock::default())).unwrap();
        assert_eq!(manager.list().unwrap().len(), 1);
        use std::io::Read;
        let mut prefix = [0; 10];
        std::fs::File::open(config.disk_path)
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert_eq!(&prefix, b"persistent");
    }
    #[test]
    fn existing_disk_is_never_truncated() {
        let temp = Temp::new();
        let config = temp.config();
        fs::write(&config.disk_path, b"precious").unwrap();
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        assert!(manager.create(config.clone()).is_err());
        assert_eq!(fs::read(config.disk_path).unwrap(), b"precious");
        assert!(manager.list().unwrap().is_empty());
    }
    #[test]
    fn invalid_config_does_not_create_disk_or_registry() {
        let temp = Temp::new();
        let mut config = temp.config();
        config.cpus = 0;
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        assert!(manager.create(config.clone()).is_err());
        assert!(!config.disk_path.exists());
        assert!(manager.get("../escape").is_err());
    }
    #[test]
    fn prepared_disk_can_be_imported_but_never_shared() {
        let temp = Temp::new();
        let config = temp.config();
        let mut disk = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.disk_path)
            .unwrap();
        disk.set_len(1024 * 1024 * 1024).unwrap();
        disk.write_all(b"prepared root").unwrap();
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        let mut duplicate = config.clone();
        duplicate.id = "second".into();
        duplicate.name = "Second Linux".into();
        assert!(manager.create(duplicate).is_err());
        use std::io::Read;
        let mut prefix = [0; 13];
        File::open(config.disk_path)
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert_eq!(&prefix, b"prepared root");
    }
    #[cfg(unix)]
    #[test]
    fn disk_aliases_cannot_bypass_exclusive_attachment() {
        let temp = Temp::new();
        let config = temp.config();
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        let mut alias = config.clone();
        alias.id = "alias".into();
        alias.name = "Alias Linux".into();
        alias.disk_path = temp.0.join("alias.raw");
        std::os::unix::fs::symlink(&config.disk_path, &alias.disk_path).unwrap();
        assert!(manager.create(alias.clone()).is_err());
        fs::remove_file(&alias.disk_path).unwrap();
        fs::hard_link(&config.disk_path, &alias.disk_path).unwrap();
        assert!(manager.create(alias).is_err());
    }
    #[test]
    fn boot_mode_updates_are_stopped_only_and_preserve_disk() {
        let temp = Temp::new();
        let mut config = temp.config();
        let iso = temp.0.join("installer.iso");
        fs::write(&iso, b"installer").unwrap();
        config.iso_path = Some(iso.clone());
        config.kernel_path.clear();
        let registry = temp.0.join("registry");
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        let mut disk = OpenOptions::new()
            .write(true)
            .open(&config.disk_path)
            .unwrap();
        disk.write_all(b"installed root").unwrap();
        let installer = manager
            .update_boot_mode("linux", BootMode::Installer)
            .unwrap();
        assert_eq!(installer.config.boot_mode, BootMode::Installer);
        manager.start("linux").unwrap();
        let before = fs::read(registry.join("linux.json")).unwrap();
        assert!(manager.update_boot_mode("linux", BootMode::Disk).is_err());
        assert_eq!(fs::read(registry.join("linux.json")).unwrap(), before);
        manager.stop("linux").unwrap();
        fs::remove_file(&iso).unwrap();
        manager.update_boot_mode("linux", BootMode::Disk).unwrap();
        drop(manager);
        let manager = VmManager::new(&registry, Arc::new(Mock::default())).unwrap();
        let saved = manager.get("linux").unwrap();
        assert_eq!(saved.config.boot_mode, BootMode::Disk);
        assert_eq!(saved.config.iso_path, Some(iso));
        assert_eq!(manager.start("linux").unwrap().state, VmState::Running);
        manager.stop("linux").unwrap();
        let before = fs::read(registry.join("linux.json")).unwrap();
        assert!(manager
            .update_boot_mode("linux", BootMode::Installer)
            .is_err());
        assert_eq!(fs::read(registry.join("linux.json")).unwrap(), before);
        use std::io::Read;
        let mut marker = [0; 14];
        File::open(config.disk_path)
            .unwrap()
            .read_exact(&mut marker)
            .unwrap();
        assert_eq!(&marker, b"installed root");
    }
    #[test]
    fn registry_rejects_simultaneous_owner() {
        let temp = Temp::new();
        let first = VmManager::new(&temp.0, Arc::new(Mock::default())).unwrap();
        assert!(VmManager::new(&temp.0, Arc::new(Mock::default())).is_err());
        drop(first);
        assert!(VmManager::new(&temp.0, Arc::new(Mock::default())).is_ok());
    }
    #[test]
    fn missing_disk_prevents_start() {
        let temp = Temp::new();
        let config = temp.config();
        let manager = VmManager::new(temp.0.join("registry"), Arc::new(Mock::default())).unwrap();
        manager.create(config.clone()).unwrap();
        fs::remove_file(config.disk_path).unwrap();
        assert!(matches!(
            manager.start("linux"),
            Err(VmError::InvalidConfig(_))
        ));
    }
}
