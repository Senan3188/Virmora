//! QEMU Linux direct-boot backend with persistent disks and user-mode NAT.
use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
pub mod metrics;
mod tpm;
pub mod vmnet;
pub use metrics::VmMetrics;
use vm_core::{
    BackendKind, GuestArchitecture, GuestOs, NetworkMode, VmBackend, VmConfig, VmError, VmState,
};

#[cfg(unix)]
mod internal_network;

struct Instance {
    child: Child,
    failed: bool,
    monitor: Option<metrics::Monitor>,
    tpm: Option<tpm::Tpm>,
}

pub struct QemuBackend {
    runtime_dir: PathBuf,
    bundle_contents: Option<PathBuf>,
    instances: Mutex<HashMap<String, Instance>>,
    display: bool,
    internal_socket: PathBuf,
    #[cfg(unix)]
    internal_network: Mutex<Option<internal_network::InternalNetwork>>,
}

impl QemuBackend {
    pub fn new(runtime_dir: PathBuf) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let internal_socket = runtime_dir
            .join(format!("lan-{}-{sequence}", std::process::id()))
            .join("lan.sock");
        Self {
            runtime_dir,
            bundle_contents: None,
            internal_socket,
            #[cfg(unix)]
            internal_network: Mutex::new(None),
            instances: Mutex::new(HashMap::new()),
            display: cfg!(target_os = "macos"),
        }
    }
    /// Use the runtime shipped inside a macOS app bundle.
    pub fn with_bundle_contents(mut self, contents: PathBuf) -> Self {
        self.bundle_contents = Some(contents);
        self
    }

    fn runtime_binary(&self, name: &str) -> PathBuf {
        match &self.bundle_contents {
            Some(contents) => contents.join("MacOS").join(name),
            None => PathBuf::from(name),
        }
    }

    fn bundled_data(&self) -> Option<PathBuf> {
        self.bundle_contents
            .as_ref()
            .map(|contents| contents.join("Resources/qemu"))
    }

    pub fn check_network(&self, mode: NetworkMode, adapter: &str) -> vmnet::NetworkReadiness {
        vmnet::readiness_with_runtime(
            mode,
            adapter,
            |name| self.runtime_binary(name),
            self.bundled_data().as_deref(),
        )
    }
    /// Enable the native QEMU Cocoa display for firmware boots (macOS only).
    pub fn with_display(runtime_dir: PathBuf, enabled: bool) -> Self {
        let mut backend = Self::new(runtime_dir);
        backend.display = enabled;
        backend
    }
    pub fn nvram_path(config: &VmConfig) -> PathBuf {
        let mut path = config.disk_path.as_os_str().to_os_string();
        path.push(".nvram.fd");
        PathBuf::from(path)
    }
    fn firmware_paths(&self, config: &VmConfig) -> Result<(PathBuf, PathBuf), VmError> {
        if config.guest_os == GuestOs::Windows11 {
            let default_folder = self.bundled_data().or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join("Library/Application Support/Virmora/Firmware"))
            });
            let resolve = |path: &Option<PathBuf>, filename: &str| {
                path.clone().or_else(|| default_folder.as_ref().map(|folder| folder.join(filename)))
                    .filter(|path| path.is_file())
                    .ok_or_else(|| VmError::Backend(format!(
                        "Windows ARM64 firmware {filename} is missing. Run scripts/setup-windows-firmware.sh or provide the firmware paths."
                    )))
            };
            return Ok((
                resolve(&config.firmware_code_path, "AAVMF_CODE.secboot.fd")?,
                resolve(&config.firmware_vars_template_path, "AAVMF_VARS.ms.fd")?,
            ));
        }
        let (code_name, vars_name) = match config.architecture {
            GuestArchitecture::Aarch64 => ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"),
            GuestArchitecture::X86_64 => ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
        };
        let discover = |name: &str| -> Result<PathBuf, VmError> {
            if let Some(folder) = self.bundled_data() {
                let path = folder.join(name);
                return if path.is_file() {
                    Ok(path)
                } else {
                    Err(VmError::Backend(format!(
                        "Bundled UEFI firmware is missing: {}",
                        path.display()
                    )))
                };
            }
            ["/opt/homebrew/share/qemu", "/usr/local/share/qemu", "/usr/share/qemu"].iter()
                .map(|root| Path::new(root).join(name)).find(|p| p.is_file())
                .ok_or_else(|| VmError::Backend(format!("UEFI firmware {name} was not found; install QEMU or configure explicit firmware paths")))
        };
        let code = match &config.firmware_code_path {
            Some(p) => p.clone(),
            None => discover(code_name)?,
        };
        let vars = match &config.firmware_vars_template_path {
            Some(p) => p.clone(),
            None => discover(vars_name)?,
        };
        Ok((code, vars))
    }
    fn prepare_nvram(&self, config: &VmConfig) -> Result<(), VmError> {
        let (_, template) = self.firmware_paths(config)?;
        let path = Self::nvram_path(config);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut output) => {
                let result = (|| -> std::io::Result<()> {
                    let mut input = fs::File::open(template)?;
                    std::io::copy(&mut input, &mut output)?;
                    output.sync_all()
                })();
                if let Err(error) = result {
                    let _ = fs::remove_file(path);
                    return Err(error.into());
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.file_type().is_file()
                    || metadata.len() == 0
                    || metadata.len() != fs::metadata(&template)?.len()
                {
                    return Err(VmError::InvalidConfig(
                        "Existing VM NVRAM is invalid; refusing to overwrite it".into(),
                    ));
                }
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Execute a QMP request such as screendump, send-key, or system_reset.
    pub fn qmp_execute(
        &self,
        id: &str,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        qmp_request(&self.qmp_path(id)?, request)
    }
    pub fn metrics(&self, id: &str) -> Result<VmMetrics, VmError> {
        self.directory(id)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        let Some(instance) = instances.get_mut(id) else {
            return Ok(VmMetrics::stopped(0));
        };
        if instance.child.try_wait()?.is_some() {
            let generation = instance.monitor.as_ref().map(|m| m.generation).unwrap_or(0);
            instance.monitor = None;
            instance.tpm = None;
            return Ok(VmMetrics::stopped(generation));
        }
        let pid = instance.child.id();
        match &mut instance.monitor {
            Some(monitor) => Ok(monitor.sample(pid)),
            None => {
                let mut sample = VmMetrics::stopped(0);
                sample.running = true;
                sample.pid = Some(pid);
                Ok(sample)
            }
        }
    }
    fn directory(&self, id: &str) -> Result<PathBuf, VmError> {
        vm_core::validate_id(id)?;
        Ok(self.runtime_dir.join(id))
    }
    pub fn serial_socket_path(&self, id: &str) -> Result<PathBuf, VmError> {
        Ok(self.directory(id)?.join("serial.sock"))
    }
    /// QEMU process diagnostics, separate from guest serial output.
    pub fn log_path(&self, id: &str) -> Result<PathBuf, VmError> {
        Ok(self.directory(id)?.join("qemu.log"))
    }
    fn qmp_path(&self, id: &str) -> Result<PathBuf, VmError> {
        Ok(self.directory(id)?.join("qmp.sock"))
    }
    fn network_arguments(&self, config: &VmConfig) -> Result<Vec<OsString>, VmError> {
        let mode = config.network_mode;
        let backend = match mode {
            NetworkMode::Nat => "user,id=net0".into(),
            NetworkMode::Disconnected => return Ok(vec!["-nic".into(), "none".into()]),
            NetworkMode::Internal if cfg!(unix) => format!(
                "stream,id=net0,addr.type=unix,addr.path={},server=off",
                escape_option(&self.internal_socket)?
            ),
            NetworkMode::NatNetwork if cfg!(target_os = "macos") => "vmnet-shared,id=net0".into(),
            NetworkMode::HostOnly if cfg!(target_os = "macos") => "vmnet-host,id=net0".into(),
            NetworkMode::Bridged if cfg!(target_os = "macos") => format!(
                "vmnet-bridged,id=net0,ifname={}",
                config
                    .bridge_interface
                    .as_deref()
                    .unwrap_or("en0")
                    .replace(',', ",,")
            ),
            _ => {
                return Err(VmError::Backend(format!(
                    "Network mode {mode:?} is unavailable on this host"
                )))
            }
        };
        let mut device = String::from("virtio-net-pci,netdev=net0");
        if config.architecture == GuestArchitecture::Aarch64 {
            device.push_str(",romfile=");
        }
        // Preserve the legacy NAT MAC. Shared networks need distinct addresses across VMs.
        if mode != NetworkMode::Nat {
            let hash = config
                .id
                .bytes()
                .fold(0xcbf29ce484222325_u64, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                })
                .to_be_bytes();
            device.push_str(&format!(
                ",mac=02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                hash[3], hash[4], hash[5], hash[6], hash[7]
            ));
        }
        Ok(vec![
            "-netdev".into(),
            backend.into(),
            "-device".into(),
            device.into(),
        ])
    }

    /// Build arguments without a shell, preserving paths containing spaces.
    pub fn command_arguments(&self, config: &VmConfig) -> Result<(String, Vec<OsString>), VmError> {
        let windows = config.guest_os == GuestOs::Windows11;
        if windows && !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(VmError::Backend(
                "This Windows profile requires an Apple Silicon Mac".into(),
            ));
        }
        let networking = self.network_arguments(config)?;
        let dir = self.directory(&config.id)?;
        #[cfg(unix)]
        for name in [
            "serial.sock",
            "qmp.sock",
            if windows { "tpm.sock" } else { "qmp.sock" },
        ] {
            use std::os::unix::ffi::OsStrExt;
            if dir.join(name).as_os_str().as_bytes().len() > 103 {
                return Err(VmError::InvalidConfig(
                    "Runtime socket path exceeds 103 bytes; choose a shorter runtime directory"
                        .into(),
                ));
            }
        }
        let native = match config.architecture {
            GuestArchitecture::Aarch64 => cfg!(target_arch = "aarch64"),
            GuestArchitecture::X86_64 => cfg!(target_arch = "x86_64"),
        };
        let accelerated = cfg!(target_os = "macos") && native;
        let (binary, machine, cpu, console) = match config.architecture {
            GuestArchitecture::Aarch64 => (
                "qemu-system-aarch64",
                "virt,gic-version=3",
                if accelerated { "host" } else { "max" },
                "ttyAMA0",
            ),
            GuestArchitecture::X86_64 => (
                "qemu-system-x86_64",
                "q35",
                if accelerated { "host" } else { "max" },
                "ttyS0",
            ),
        };
        let mut args: Vec<OsString> = vec![
            "-name".into(),
            format!("guest={}", config.name.replace(',', ",,")).into(),
            "-machine".into(),
            if windows {
                "virt,gic-version=2,acpi=on".into()
            } else {
                machine.into()
            },
            "-accel".into(),
            if windows {
                // The ARM64 Windows boot manager stalls with the default GICv3
                // on the tested Apple Silicon host. GICv2 needs userspace IRQs.
                "hvf,kernel-irqchip=off".into()
            } else if accelerated {
                "hvf".into()
            } else {
                "tcg".into()
            },
            "-cpu".into(),
            cpu.into(),
            "-smp".into(),
            config.cpus.to_string().into(),
            "-m".into(),
            config.memory_mib.to_string().into(),
            "-display".into(),
            if !config.direct_kernel_boot() && self.display && cfg!(target_os = "macos") {
                "cocoa".into()
            } else {
                "none".into()
            },
            "-monitor".into(),
            "none".into(),
        ];
        if let Some(data) = self.bundled_data() {
            args.extend(["-L".into(), data.into_os_string()]);
        }
        if config.direct_kernel_boot() {
            args.extend([
                "-kernel".into(),
                config.kernel_path.as_os_str().into(),
                "-no-reboot".into(),
            ]);
            if let Some(initrd) = &config.initrd_path {
                args.extend(["-initrd".into(), initrd.as_os_str().into()]);
            }
            let cmdline = if config
                .kernel_cmdline
                .split_whitespace()
                .any(|s| s.starts_with("console="))
            {
                config.kernel_cmdline.clone()
            } else {
                format!("console={console} {}", config.kernel_cmdline)
            };
            args.extend(["-append".into(), cmdline.into()]);
        } else {
            if config.architecture == GuestArchitecture::X86_64 {
                // q35 otherwise creates its own default VGA adapter in addition
                // to the explicitly configured virtio GPU.
                args.extend(["-vga".into(), "none".into()]);
            }
            let (code, _) = self.firmware_paths(config)?;
            args.extend([
                "-drive".into(),
                format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    escape_option(&code)?
                )
                .into(),
                "-drive".into(),
                format!(
                    "if=pflash,format=raw,unit=1,file={}",
                    escape_option(&Self::nvram_path(config))?
                )
                .into(),
                "-device".into(),
                if windows {
                    // Keep firmware graphics available to Windows Setup before
                    // a guest display driver has been installed.
                    "ramfb".into()
                } else {
                    "virtio-gpu-pci,id=display0,addr=0x6".into()
                },
                "-device".into(),
                "qemu-xhci,id=usb0,addr=0x7".into(),
                "-device".into(),
                "usb-kbd".into(),
                "-device".into(),
                "usb-tablet".into(),
                "-drive".into(),
                format!(
                    "file={},format=raw,if=none,id=osdisk",
                    escape_option(&config.disk_path)?
                )
                .into(),
                "-device".into(),
                format!(
                    "{},drive=osdisk,id=osdisk-device,addr=0x9,bootindex={}",
                    if windows {
                        "nvme,serial=VIRMORA0001"
                    } else {
                        "virtio-blk-pci"
                    },
                    if config.installer_boot() { 2 } else { 1 }
                )
                .into(),
            ]);
            if windows {
                args.extend([
                    "-chardev".into(),
                    format!(
                        "socket,id=tpm-control,path={}",
                        escape_option(&dir.join("tpm.sock"))?
                    )
                    .into(),
                    "-tpmdev".into(),
                    "emulator,id=tpm0,chardev=tpm-control".into(),
                    "-device".into(),
                    "tpm-tis-device,tpmdev=tpm0,ppi=off".into(),
                ]);
                if let Some(drivers) = &config.drivers_iso_path {
                    args.extend([
                        "-drive".into(),
                        format!(
                            "file={},format=raw,media=cdrom,readonly=on,if=none,id=drivers",
                            escape_option(drivers)?
                        )
                        .into(),
                        "-device".into(),
                        "usb-storage,drive=drivers,bus=usb0.0,port=4,id=windows-drivers".into(),
                    ]);
                }
            } else {
                // Keep the controller at the same PCI address across Linux boot modes.
                args.extend(["-device".into(), "virtio-scsi-pci,id=scsi0,addr=0x8".into()]);
            }
            if config.installer_boot() {
                let iso = config.iso_path.as_ref().ok_or_else(|| {
                    VmError::InvalidConfig("Installer boot requires an ISO".into())
                })?;
                args.extend([
                    "-drive".into(),
                    format!(
                        "file={},format=raw,media=cdrom,readonly=on,if=none,id=cd0",
                        escape_option(iso)?
                    )
                    .into(),
                    "-device".into(),
                    if windows {
                        "usb-storage,drive=cd0,bus=usb0.0,port=3,id=installer-cd,bootindex=1".into()
                    } else {
                        "scsi-cd,drive=cd0,bus=scsi0.0,id=installer-cd,bootindex=1".into()
                    },
                ]);
            }
        }
        if config.direct_kernel_boot() {
            args.extend([
                "-drive".into(),
                format!(
                    "file={},format=raw,if=virtio",
                    escape_option(&config.disk_path)?
                )
                .into(),
            ]);
        }
        args.extend([
            "-chardev".into(),
            format!(
                "socket,id=console,path={},server=on,wait=off,logfile={},logappend=on",
                escape_option(&dir.join("serial.sock"))?,
                escape_option(&dir.join("console.log"))?
            )
            .into(),
            "-serial".into(),
            "chardev:console".into(),
            "-qmp".into(),
            format!(
                "unix:{},server=on,wait=off",
                escape_option(&dir.join("qmp.sock"))?
            )
            .into(),
        ]);
        args.extend(networking);
        Ok((binary.into(), args))
    }
}

fn escape_option(path: &Path) -> Result<String, VmError> {
    let path = path
        .to_str()
        .ok_or_else(|| VmError::InvalidConfig("QEMU paths must be UTF-8".into()))?;
    Ok(path.replace(',', ",,"))
}

impl VmBackend for QemuBackend {
    fn start(&self, config: &VmConfig) -> Result<(), VmError> {
        config.validate()?;
        if config.backend != BackendKind::Qemu {
            return Err(VmError::InvalidConfig(
                "QEMU backend requires backend=qemu".into(),
            ));
        }
        let (binary, args) = self.command_arguments(config)?;
        if config.cpus == 0 || config.memory_mib < 128 {
            return Err(VmError::InvalidConfig(
                "At least one CPU and 128 MiB RAM are required".into(),
            ));
        }
        if !config.direct_kernel_boot() {
            self.prepare_nvram(config)?;
        }
        for path in std::iter::once(&config.disk_path) {
            if !path.is_file() {
                return Err(VmError::InvalidConfig(format!(
                    "File does not exist: {}",
                    path.display()
                )));
            }
        }
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        if let Some(instance) = instances.get_mut(&config.id) {
            if instance.child.try_wait()?.is_none() {
                return Err(VmError::Backend("VM is already running".into()));
            }
        }
        instances.remove(&config.id);
        let dir = self.directory(&config.id)?;
        fs::create_dir_all(&dir)?;
        for socket in [dir.join("qmp.sock"), dir.join("serial.sock")] {
            if socket.exists() {
                fs::remove_file(socket)?;
            }
        }
        #[cfg(unix)]
        if config.network_mode == NetworkMode::Internal {
            use std::os::unix::ffi::OsStrExt;
            if self.internal_socket.as_os_str().as_bytes().len() > 103 {
                return Err(VmError::InvalidConfig(
                    "Internal Network socket path is too long. Choose a shorter Virmora data directory.".into(),
                ));
            }
            let mut network = self
                .internal_network
                .lock()
                .map_err(|_| VmError::Backend("Internal Network lock poisoned".into()))?;
            if network.is_none() {
                *network = Some(internal_network::InternalNetwork::start(
                    &self.internal_socket,
                )?);
            }
        }
        let errors = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("qemu.log"))?;
        let socket = vmnet::socket_path(
            config.network_mode,
            config.bridge_interface.as_deref().unwrap_or("en0"),
        );
        let tpm = if config.guest_os == GuestOs::Windows11 {
            Some(tpm::Tpm::start(
                &self.runtime_binary("swtpm"),
                &dir.join("tpm.sock"),
                &config.tpm_state_path(),
                &dir.join("qemu.log"),
            )?)
        } else {
            None
        };
        let executable = self.runtime_binary(&binary);
        let child = vmnet::command(&executable, args, socket.as_deref())?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(errors)
            .spawn()
            .map_err(|error| {
                VmError::Backend(format!(
                    "Could not launch {}: {error}",
                    executable.display()
                ))
            })?;
        instances.insert(
            config.id.clone(),
            Instance {
                child,
                failed: false,
                monitor: None,
                tpm,
            },
        );
        // A running child is insufficient: QEMU can fail while binding its
        // sockets or initializing devices. Wait until QMP can answer a command.
        let deadline = Instant::now() + Duration::from_secs(3);
        let instance = instances.get_mut(&config.id).unwrap();
        loop {
            if let Some(status) = instance.child.try_wait()? {
                instance.tpm = None;
                instance.failed = true;
                let hint = match config.network_mode {
                    NetworkMode::NatNetwork | NetworkMode::HostOnly | NetworkMode::Bridged =>
                        " The selected mode requires its socket_vmnet service or a QEMU build authorized for macOS vmnet. Check the log for permission or interface errors.",
                    _ => "",
                };
                return Err(VmError::Backend(format!(
                    "QEMU exited during startup ({status}).{hint} Log: {}",
                    dir.join("qemu.log").display()
                )));
            }
            if qmp_ready(&dir.join("qmp.sock")) {
                instance.monitor = Some(metrics::Monitor::new(
                    instance.child.id(),
                    config.network_mode,
                ));
                return Ok(());
            }
            if Instant::now() >= deadline {
                let _ = instance.child.kill();
                let _ = instance.child.wait();
                instance.tpm = None;
                instance.failed = true;
                return Err(VmError::Backend(format!(
                    "QEMU did not become ready within 3 seconds; see {}",
                    dir.join("qemu.log").display()
                )));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn shutdown(&self, id: &str) -> Result<(), VmError> {
        let socket = self.qmp_path(id)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        let Some(instance) = instances.get_mut(id) else {
            return Ok(());
        };
        if instance.child.try_wait()?.is_some() {
            instances.remove(id);
            return Ok(());
        }
        // Acknowledges only the ACPI request. Guest cooperation determines when
        // it exits; this method never waits for exit or sends a kill signal.
        qmp_request(&socket, serde_json::json!({"execute":"system_powerdown"}))?;
        Ok(())
    }
    fn force_stop(&self, id: &str) -> Result<(), VmError> {
        self.directory(id)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        let Some(instance) = instances.get_mut(id) else {
            return Ok(());
        };
        if instance.child.try_wait()?.is_none() {
            if let Err(error) = instance.child.kill() {
                // The process may have exited between try_wait and kill.
                if instance.child.try_wait()?.is_none() {
                    return Err(error.into());
                }
            }
            instance.child.wait()?;
        }
        // Dropping the instance also kills/reaps its network collector and
        // joins its reader; the stopped state never inherits a failed status.
        instances.remove(id);
        Ok(())
    }
    fn stop(&self, id: &str) -> Result<(), VmError> {
        let socket = self.qmp_path(id)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        let Some(instance) = instances.get_mut(id) else {
            return Ok(());
        };
        if instance.child.try_wait()?.is_some() {
            instances.remove(id);
            return Ok(());
        }
        let _ = request_shutdown(&socket);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if instance.child.try_wait()?.is_some() {
                instances.remove(id);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        instance.child.kill()?;
        instance.child.wait()?;
        instances.remove(id);
        Ok(())
    }
    fn state(&self, id: &str) -> Result<VmState, VmError> {
        self.directory(id)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| VmError::Backend("VM process lock poisoned".into()))?;
        let Some(instance) = instances.get_mut(id) else {
            return Ok(VmState::Stopped);
        };
        match instance.child.try_wait()? {
            None => Ok(VmState::Running),
            Some(status) => {
                instance.monitor = None;
                instance.tpm = None;
                instance.failed |= !status.success();
                Ok(if instance.failed {
                    VmState::Failed
                } else {
                    VmState::Stopped
                })
            }
        }
    }
    fn console_path(&self, id: &str) -> Result<PathBuf, VmError> {
        Ok(self.directory(id)?.join("console.log"))
    }
}

#[cfg(unix)]
fn qmp_request(path: &Path, request: serde_json::Value) -> Result<serde_json::Value, VmError> {
    qmp_request_with_timeout(path, request, Duration::from_secs(3))
}

#[cfg(unix)]
fn qmp_request_with_timeout(
    path: &Path,
    mut request: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, VmError> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    fn receive(
        reader: &mut BufReader<UnixStream>,
        expected: &str,
    ) -> Result<serde_json::Value, VmError> {
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(VmError::Backend("QMP connection closed".into()));
            }
            let value: serde_json::Value = serde_json::from_str(&line)?;
            if value.get("id").and_then(|v| v.as_str()) == Some(expected) {
                if let Some(error) = value.get("error") {
                    return Err(VmError::Backend(format!("QMP: {error}")));
                }
                return Ok(value
                    .get("return")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
        }
    }
    let mut greeting = String::new();
    reader.read_line(&mut greeting)?;
    let greeting: serde_json::Value = serde_json::from_str(&greeting)?;
    if greeting.get("QMP").is_none() {
        return Err(VmError::Backend("Invalid QMP greeting".into()));
    }
    stream.write_all(b"{\"execute\":\"qmp_capabilities\",\"id\":\"capabilities\"}\n")?;
    receive(&mut reader, "capabilities")?;
    let object = request
        .as_object_mut()
        .ok_or_else(|| VmError::InvalidConfig("QMP request must be an object".into()))?;
    object.insert("id".into(), "request".into());
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    receive(&mut reader, "request")
}
#[cfg(not(unix))]
fn qmp_request(_: &Path, _: serde_json::Value) -> Result<serde_json::Value, VmError> {
    Err(VmError::Backend(
        "QMP Unix sockets require a Unix host".into(),
    ))
}

#[cfg(unix)]
fn qmp_ready(path: &Path) -> bool {
    qmp_request_with_timeout(
        path,
        serde_json::json!({"execute": "query-status"}),
        Duration::from_millis(100),
    )
    .is_ok()
}
#[cfg(not(unix))]
fn qmp_ready(_: &Path) -> bool {
    false
}

#[cfg(unix)]
fn request_shutdown(path: &Path) -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\n")?;
    line.clear();
    reader.read_line(&mut line)?;
    stream.write_all(b"{\"execute\":\"system_powerdown\"}\n")?;
    Ok(())
}
#[cfg(not(unix))]
fn request_shutdown(_: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Unix sockets required",
    ))
}

impl Drop for QemuBackend {
    fn drop(&mut self) {
        let instances = self.instances.get_mut().unwrap_or_else(|e| e.into_inner());
        for instance in instances.values_mut() {
            if matches!(instance.child.try_wait(), Ok(None)) {
                let _ = instance.child.kill();
                let _ = instance.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    fn config(architecture: GuestArchitecture) -> VmConfig {
        VmConfig {
            guest_os: vm_core::GuestOs::Linux,
            drivers_iso_path: None,
            id: "linux".into(),
            name: "Linux".into(),
            cpus: 2,
            memory_mib: 1024,
            disk_path: "/tmp/my disk.raw".into(),
            disk_size_gib: 8,
            kernel_path: "/tmp/Image".into(),
            initrd_path: Some("/tmp/initrd".into()),
            kernel_cmdline: "root=/dev/vda rw".into(),
            architecture,
            iso_path: None,
            boot_mode: vm_core::BootMode::Auto,
            firmware_code_path: None,
            firmware_vars_template_path: None,
            backend: BackendKind::Qemu,
            network_mode: vm_core::NetworkMode::Nat,
            bridge_interface: None,
        }
    }
    fn firmware_config() -> (VmConfig, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "virmora-firmware-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut cfg = config(GuestArchitecture::Aarch64);
        cfg.kernel_path = PathBuf::new();
        cfg.initrd_path = None;
        cfg.disk_path = root.join("disk.raw");
        cfg.firmware_code_path = Some(root.join("code.fd"));
        cfg.firmware_vars_template_path = Some(root.join("vars.fd"));
        fs::write(cfg.firmware_code_path.as_ref().unwrap(), b"firmware").unwrap();
        fs::write(
            cfg.firmware_vars_template_path.as_ref().unwrap(),
            b"initial variables",
        )
        .unwrap();
        (cfg, root)
    }
    #[test]
    fn firmware_installer_and_disk_boot_have_separate_media() {
        let (mut cfg, root) = firmware_config();
        cfg.boot_mode = vm_core::BootMode::Installer;
        cfg.iso_path = Some(root.join("installer.iso"));
        fs::write(cfg.iso_path.as_ref().unwrap(), b"ISO").unwrap();
        cfg.validate().unwrap();
        let backend = QemuBackend::with_display("/tmp/virmora".into(), false);
        let (_, args) = backend.command_arguments(&cfg).unwrap();
        assert!(!args.contains(&OsString::from("-kernel")));
        assert!(args.contains(&OsString::from("virtio-scsi-pci,id=scsi0,addr=0x8")));
        assert!(args.contains(&OsString::from(
            "virtio-blk-pci,drive=osdisk,id=osdisk-device,addr=0x9,bootindex=2"
        )));
        assert!(args
            .iter()
            .any(|arg| arg.to_string_lossy().contains("media=cdrom,readonly=on")));
        assert!(args
            .iter()
            .any(|arg| arg.to_string_lossy().contains("unit=0,readonly=on")));
        assert!(args.contains(&OsString::from("virtio-gpu-pci,id=display0,addr=0x6")));
        cfg.boot_mode = vm_core::BootMode::Disk;
        fs::remove_file(cfg.iso_path.as_ref().unwrap()).unwrap();
        cfg.validate().unwrap();
        let (_, args) = backend.command_arguments(&cfg).unwrap();
        assert!(!args
            .iter()
            .any(|arg| arg.to_string_lossy().contains("media=cdrom")));
        assert!(!args.contains(&OsString::from("-boot")));
        cfg.architecture = GuestArchitecture::X86_64;
        let (_, args) = backend.command_arguments(&cfg).unwrap();
        assert!(args
            .windows(2)
            .any(|args| args == [OsString::from("-vga"), OsString::from("none")]));
        assert!(args.contains(&OsString::from("virtio-scsi-pci,id=scsi0,addr=0x8")));
        assert!(args.contains(&OsString::from(
            "virtio-blk-pci,drive=osdisk,id=osdisk-device,addr=0x9,bootindex=1"
        )));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_runtime_resolves_firmware_and_executables_inside_the_bundle() {
        let (mut cfg, root) = firmware_config();
        let contents = root.join("Virmora Test.app/Contents");
        let data = contents.join("Resources/qemu");
        fs::create_dir_all(&data).unwrap();
        cfg.firmware_code_path = None;
        cfg.firmware_vars_template_path = None;
        let code = data.join("edk2-aarch64-code.fd");
        let vars = data.join("edk2-arm-vars.fd");
        fs::write(&code, b"firmware").unwrap();
        fs::write(&vars, b"variables").unwrap();
        let backend =
            QemuBackend::new("/tmp/virmora".into()).with_bundle_contents(contents.clone());
        assert_eq!(backend.firmware_paths(&cfg).unwrap(), (code, vars));
        let (binary, args) = backend.command_arguments(&cfg).unwrap();
        assert_eq!(
            backend.runtime_binary(&binary),
            contents.join("MacOS").join(binary)
        );
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-L" && pair[1] == data.as_os_str()));
        assert_eq!(
            backend.runtime_binary("swtpm"),
            contents.join("MacOS/swtpm")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn windows_installer_and_disk_boot_use_the_same_guest_hardware() {
        let (mut cfg, root) = firmware_config();
        cfg.guest_os = GuestOs::Windows11;
        cfg.memory_mib = 4096;
        cfg.disk_size_gib = 64;
        cfg.boot_mode = vm_core::BootMode::Installer;
        cfg.iso_path = Some(root.join("windows.iso"));
        cfg.drivers_iso_path = Some(root.join("drivers.iso"));
        fs::write(cfg.iso_path.as_ref().unwrap(), b"installer").unwrap();
        fs::write(cfg.drivers_iso_path.as_ref().unwrap(), b"drivers").unwrap();
        cfg.validate().unwrap();
        let backend = QemuBackend::with_display("/tmp/virmora".into(), false);
        let (_, installer) = backend.command_arguments(&cfg).unwrap();

        cfg.boot_mode = vm_core::BootMode::Disk;
        fs::remove_file(cfg.iso_path.as_ref().unwrap()).unwrap();
        cfg.validate().unwrap();
        let (_, disk) = backend.command_arguments(&cfg).unwrap();

        for args in [&installer, &disk] {
            assert!(args.contains(&"virt,gic-version=2,acpi=on".into()));
            assert!(args.contains(&"hvf,kernel-irqchip=off".into()));
            assert!(args.contains(&"ramfb".into()));
            assert!(args.contains(&"tpm-tis-device,tpmdev=tpm0,ppi=off".into()));
            assert!(args.contains(
                &"usb-storage,drive=drivers,bus=usb0.0,port=4,id=windows-drivers".into()
            ));
        }
        assert!(installer.contains(
            &"usb-storage,drive=cd0,bus=usb0.0,port=3,id=installer-cd,bootindex=1".into()
        ));
        assert!(!disk
            .iter()
            .any(|arg| arg.to_string_lossy().contains("id=cd0")));
        for (args, index) in [(installer, 2), (disk, 1)] {
            assert!(args.contains(
                &format!(
                "nvme,serial=VIRMORA0001,drive=osdisk,id=osdisk-device,addr=0x9,bootindex={index}"
            )
                .into()
            ));
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nvram_is_initialized_once_and_preserved_on_restart() {
        let (cfg, root) = firmware_config();
        let backend = QemuBackend::new(root.join("run"));
        backend.prepare_nvram(&cfg).unwrap();
        let nvram = QemuBackend::nvram_path(&cfg);
        assert_eq!(fs::read(&nvram).unwrap(), b"initial variables");
        fs::write(&nvram, b"guest variables!!").unwrap();
        backend.prepare_nvram(&cfg).unwrap();
        assert_eq!(fs::read(&nvram).unwrap(), b"guest variables!!");
        assert_ne!(nvram, cfg.disk_path);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn corrupt_nvram_is_rejected_without_replacing_it() {
        let (cfg, root) = firmware_config();
        let backend = QemuBackend::new(root.join("run"));
        let nvram = QemuBackend::nvram_path(&cfg);
        fs::write(&nvram, b"truncated").unwrap();
        assert!(backend.prepare_nvram(&cfg).is_err());
        assert_eq!(fs::read(&nvram).unwrap(), b"truncated");
        #[cfg(unix)]
        {
            fs::remove_file(&nvram).unwrap();
            std::os::unix::fs::symlink(cfg.firmware_vars_template_path.as_ref().unwrap(), &nvram)
                .unwrap();
            assert!(backend.prepare_nvram(&cfg).is_err());
            assert!(fs::symlink_metadata(&nvram)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn graphical_window_name_preserves_commas() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        let mut cfg = config(GuestArchitecture::Aarch64);
        cfg.name = "Alpine, work VM".into();
        let (_, args) = backend.command_arguments(&cfg).unwrap();
        assert!(args.windows(2).any(|args| args
            == [
                OsString::from("-name"),
                OsString::from("guest=Alpine,, work VM")
            ]));
    }
    #[test]
    fn automatic_boot_selection_and_installer_validation() {
        let (mut cfg, root) = firmware_config();
        assert!(!cfg.direct_kernel_boot());
        assert!(!cfg.installer_boot());
        cfg.boot_mode = vm_core::BootMode::Installer;
        assert!(cfg.validate().is_err());
        cfg.boot_mode = vm_core::BootMode::Auto;
        cfg.iso_path = Some(root.join("missing.iso"));
        assert!(cfg.installer_boot());
        assert!(cfg.validate().is_err());
        cfg.boot_mode = vm_core::BootMode::Disk;
        cfg.validate().unwrap();
        cfg.boot_mode = vm_core::BootMode::Auto;
        cfg.kernel_path = cfg.firmware_code_path.clone().unwrap();
        assert!(cfg.direct_kernel_boot());
        assert!(!cfg.installer_boot());
        cfg.validate().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn network_selection_is_applied_without_silent_nat_fallback() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        let mut cfg = config(GuestArchitecture::Aarch64);
        cfg.network_mode = vm_core::NetworkMode::Disconnected;
        let (_, args) = backend.command_arguments(&cfg).unwrap();
        assert!(args
            .windows(2)
            .any(|a| a == [OsString::from("-nic"), OsString::from("none")]));
        assert!(!args.contains(&OsString::from("-netdev")));
        for (mode, prefix) in [
            (NetworkMode::Internal, "stream,id=net0,addr.type=unix,"),
            (NetworkMode::NatNetwork, "vmnet-shared,id=net0"),
            (NetworkMode::HostOnly, "vmnet-host,id=net0"),
            (NetworkMode::Bridged, "vmnet-bridged,id=net0,ifname=en3"),
        ] {
            cfg.network_mode = mode;
            cfg.bridge_interface = Some("en3".into());
            if cfg!(target_os = "macos") || (cfg!(unix) && mode == NetworkMode::Internal) {
                let (_, args) = backend.command_arguments(&cfg).unwrap();
                assert!(args
                    .iter()
                    .any(|arg| arg.to_string_lossy().starts_with(prefix)));
                assert!(!args.iter().any(|arg| arg == "user,id=net0"));
            } else {
                assert!(backend.command_arguments(&cfg).is_err());
            }
        }
    }
    #[test]
    fn builds_arm_linux_boot_with_nat_and_persistent_disk() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        let (binary, args) = backend
            .command_arguments(&config(GuestArchitecture::Aarch64))
            .unwrap();
        assert_eq!(binary, "qemu-system-aarch64");
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        assert!(args.contains(&"user,id=net0".into()));
        assert!(args.contains(&"virtio-net-pci,netdev=net0,romfile=".into()));
        assert!(args.contains(&"file=/tmp/my disk.raw,format=raw,if=virtio".into()));
        assert!(args.contains(&"console=ttyAMA0 root=/dev/vda rw".into()));
        assert!(!args.contains(&"-snapshot".into()));
    }
    #[test]
    fn preserves_explicit_console() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        let mut cfg = config(GuestArchitecture::X86_64);
        cfg.kernel_cmdline = "console=ttyS0 root=/dev/vda".into();
        let (binary, args) = backend.command_arguments(&cfg).unwrap();
        assert_eq!(binary, "qemu-system-x86_64");
        assert!(args.contains(&OsString::from("console=ttyS0 root=/dev/vda")));
    }
    #[test]
    fn rejects_traversal_ids_and_escapes_commas() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        assert!(backend.console_path("../escape").is_err());
        assert_eq!(escape_option(Path::new("/tmp/a,b")).unwrap(), "/tmp/a,,b");
    }
    #[cfg(unix)]
    #[test]
    fn explicit_stop_reports_stopped_after_forced_shutdown() {
        let root = std::env::temp_dir().join(format!("virmora-stop-{}", std::process::id()));
        let backend = QemuBackend::new(root);
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        backend.instances.lock().unwrap().insert(
            "sleep".into(),
            Instance {
                child,
                failed: false,
                monitor: None,
                tpm: None,
            },
        );
        backend.stop("sleep").unwrap();
        assert_eq!(backend.state("sleep").unwrap(), VmState::Stopped);
    }
    #[cfg(unix)]
    #[test]
    fn missing_qmp_shutdown_preserves_process_and_force_stop_is_idempotent() {
        let backend = QemuBackend::new(PathBuf::from(format!(
            "/tmp/v-shutdown-{}",
            std::process::id()
        )));
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let monitor = metrics::Monitor::new(child.id(), NetworkMode::Disconnected);
        backend.instances.lock().unwrap().insert(
            "guest".into(),
            Instance {
                child,
                failed: false,
                monitor: Some(monitor),
                tpm: None,
            },
        );
        assert!(backend.shutdown("guest").is_err());
        assert_eq!(backend.state("guest").unwrap(), VmState::Running);
        backend.force_stop("guest").unwrap();
        assert_eq!(backend.state("guest").unwrap(), VmState::Stopped);
        assert!(!backend.metrics("guest").unwrap().running);
        backend.force_stop("guest").unwrap();
        backend.shutdown("guest").unwrap();
        assert!(backend.force_stop("../invalid").is_err());
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "requires Unix socket binding outside the desktop sandbox"]
    fn acknowledged_shutdown_does_not_kill_uncooperative_guest() {
        use std::os::unix::net::UnixListener;
        let root = PathBuf::from(format!("/tmp/v-qmp-{}", std::process::id()));
        let backend = QemuBackend::new(root.clone());
        fs::create_dir_all(backend.directory("guest").unwrap()).unwrap();
        let socket = backend.qmp_path("guest").unwrap();
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for reject in [false, true] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream.write_all(b"{\"QMP\":{}}\n").unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                for expected in ["qmp_capabilities", "system_powerdown"] {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["execute"], expected);
                    let response = if reject && expected == "system_powerdown" {
                        serde_json::json!({"error":{"class":"GenericError","desc":"ACPI unavailable"},"id":request["id"]})
                    } else {
                        serde_json::json!({"return":{},"id":request["id"]})
                    };
                    serde_json::to_writer(&mut stream, &response).unwrap();
                    stream.write_all(b"\n").unwrap();
                }
            }
        });
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        backend.instances.lock().unwrap().insert(
            "guest".into(),
            Instance {
                child,
                failed: false,
                monitor: None,
                tpm: None,
            },
        );
        backend.shutdown("guest").unwrap();
        assert_eq!(backend.state("guest").unwrap(), VmState::Running);
        assert!(backend
            .shutdown("guest")
            .unwrap_err()
            .to_string()
            .contains("ACPI unavailable"));
        server.join().unwrap();
        assert_eq!(backend.state("guest").unwrap(), VmState::Running);
        backend.force_stop("guest").unwrap();
        assert_eq!(backend.state("guest").unwrap(), VmState::Stopped);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_overlong_socket_paths() {
        let backend = QemuBackend::new(PathBuf::from(format!("/tmp/{}", "a".repeat(100))));
        assert!(backend
            .command_arguments(&config(GuestArchitecture::Aarch64))
            .is_err());
    }
    #[test]
    fn unknown_vm_is_stopped_and_stop_is_idempotent() {
        let backend = QemuBackend::new("/tmp/virmora".into());
        assert_eq!(backend.state("unknown").unwrap(), VmState::Stopped);
        backend.stop("unknown").unwrap();
    }
}
