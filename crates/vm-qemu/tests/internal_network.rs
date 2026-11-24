//! Opt-in test of guest-to-guest Ethernet using Alpine's ARM64 virt initramfs.
#![cfg(unix)]

use std::{
    fs,
    io::Write,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use vm_core::{BackendKind, BootMode, GuestArchitecture, NetworkMode, VmBackend, VmConfig};
use vm_qemu::QemuBackend;

fn wait_for(log: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let text = String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned();
        if text.lines().any(|line| line.trim() == marker)
            || (marker == "job control turned off" && text.contains(marker))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Missing {marker} in {}: {}",
            log.display(),
            text.chars()
                .rev()
                .take(1600)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "requires QEMU, Unix sockets, and an Alpine ARM64 virt kernel and initramfs"]
fn two_guests_exchange_traffic_on_internal_network() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Temp(PathBuf::from(format!("/tmp/virmora-lan-{nonce:x}")));
    fs::create_dir(&root.0).unwrap();
    let backend = QemuBackend::with_display(root.0.join("run"), false);
    let mut guests = Vec::new();
    for (id, ip) in [("first", 1), ("second", 2)] {
        let disk = root.0.join(format!("{id}.raw"));
        fs::File::create_new(&disk)
            .unwrap()
            .set_len(1 << 30)
            .unwrap();
        let vm = VmConfig {
            guest_os: vm_core::GuestOs::Linux,
            drivers_iso_path: None,
            id: id.into(),
            name: id.into(),
            cpus: 1,
            memory_mib: 512,
            disk_path: disk,
            disk_size_gib: 1,
            kernel_path: std::env::var_os("VIRMORA_TEST_KERNEL").unwrap().into(),
            initrd_path: std::env::var_os("VIRMORA_TEST_INITRD").map(PathBuf::from),
            kernel_cmdline: "rdinit=/bin/sh".into(),
            architecture: GuestArchitecture::Aarch64,
            backend: BackendKind::Qemu,
            boot_mode: BootMode::Auto,
            iso_path: None,
            firmware_code_path: None,
            firmware_vars_template_path: None,
            network_mode: NetworkMode::Internal,
            bridge_interface: None,
        };
        backend.start(&vm).unwrap();
        let log = backend.console_path(id).unwrap();
        wait_for(&log, "job control turned off");
        let mut console = UnixStream::connect(backend.serial_socket_path(id).unwrap()).unwrap();
        writeln!(console, "\x1b[1;1R\n/bin/busybox --install -s /bin; export PATH=/bin:/sbin TERM=dumb; mkdir -p /proc /sys /dev; mount -t proc proc /proc; mount -t sysfs sysfs /sys; mount -t devtmpfs devtmpfs /dev; modprobe virtio_pci; modprobe virtio_net; ip link set eth0 up && ip addr add 192.0.2.{ip}/24 dev eth0 && echo LAN_READY").unwrap();
        wait_for(&log, "LAN_READY");
        guests.push((console, log));
    }
    let (console, log) = &mut guests[0];
    writeln!(
        console,
        "ping -c 2 -W 3 192.0.2.2 && ! ip route | grep -q '^default ' && echo PEER_REACHED"
    )
    .unwrap();
    wait_for(log, "PEER_REACHED");
    backend.force_stop("first").unwrap();
    backend.force_stop("second").unwrap();
}
