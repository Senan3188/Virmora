//! Opt-in host integration test. Requires QEMU and Linux direct-boot assets.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use vm_core::{BackendKind, GuestArchitecture, VmBackend, VmConfig, VmState};
use vm_qemu::QemuBackend;

#[test]
#[ignore = "requires installed QEMU and VIRMORA_TEST_KERNEL (optional VIRMORA_TEST_INITRD)"]
fn boots_linux_and_preserves_disk_across_restart() {
    let root = PathBuf::from("/tmp").join(format!(
        "virmora-boot-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let disk = root.join("disk.raw");
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&disk)
        .unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    // Host-side sentinel verifies that start/stop never recreates the disk.
    file.seek(SeekFrom::Start(1024 * 1024)).unwrap();
    file.write_all(b"virmora-persistent").unwrap();
    file.sync_all().unwrap();
    let architecture = match std::env::var("VIRMORA_TEST_ARCH").as_deref() {
        Ok("x86_64") => GuestArchitecture::X86_64,
        Ok("aarch64") => GuestArchitecture::Aarch64,
        _ if cfg!(target_arch = "aarch64") => GuestArchitecture::Aarch64,
        _ => GuestArchitecture::X86_64,
    };
    let config = VmConfig {
        guest_os: vm_core::GuestOs::Linux,
        drivers_iso_path: None,
        id: "smoke".into(),
        name: "Smoke Linux".into(),
        cpus: 2,
        memory_mib: 1024,
        disk_path: disk.clone(),
        disk_size_gib: 1,
        kernel_path: PathBuf::from(
            std::env::var("VIRMORA_TEST_KERNEL").expect("set VIRMORA_TEST_KERNEL"),
        ),
        initrd_path: std::env::var_os("VIRMORA_TEST_INITRD").map(PathBuf::from),
        kernel_cmdline: std::env::var("VIRMORA_TEST_CMDLINE")
            .unwrap_or_else(|_| "rdinit=/bin/sh panic=-1".into()),
        architecture,
        iso_path: None,
        boot_mode: vm_core::BootMode::Auto,
        firmware_code_path: None,
        firmware_vars_template_path: None,
        backend: BackendKind::Qemu,
        network_mode: vm_core::NetworkMode::Nat,
        bridge_interface: None,
    };
    let backend = QemuBackend::new(root.join("runtime"));
    for _ in 0..2 {
        let log = backend.console_path("smoke").unwrap();
        let previous = fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        backend.start(&config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let data = fs::read(&log).unwrap_or_default();
            if String::from_utf8_lossy(data.get(previous as usize..).unwrap_or_default())
                .contains("Linux version")
            {
                break;
            }
            assert_eq!(
                backend.state("smoke").unwrap(),
                VmState::Running,
                "QEMU exited before Linux boot marker; inspect runtime/smoke/qemu.log"
            );
            assert!(
                Instant::now() < deadline,
                "Linux boot marker not observed; console: {}",
                String::from_utf8_lossy(&data)
            );
            thread::sleep(Duration::from_millis(100));
        }
        backend.stop("smoke").unwrap();
        file.seek(SeekFrom::Start(1024 * 1024)).unwrap();
        let mut sentinel = [0u8; 18];
        file.read_exact(&mut sentinel).unwrap();
        assert_eq!(&sentinel, b"virmora-persistent");
    }
    drop(backend);
    drop(file);
    fs::remove_dir_all(root).unwrap();
}
