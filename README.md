# Virmora

Rust VM manager with QEMU, a Tauri 2 / React desktop client, and an explicit extension point for Apple Virtualization.framework. Virmora supports Linux ISO installation, installed-disk boot, direct kernel boot, persistent storage, networking, and graphical and serial consoles. The Windows 11 ARM64 preview adds an ISO installation profile for Apple Silicon Macs.

## Install Virmora on macOS

The tested installer is a self-contained Apple Silicon build for macOS 26 or later. Virmora’s DMG includes QEMU, firmware, and the TPM emulator, so users can install it without Homebrew or Xcode. Bring a Linux or Windows installer ISO for the guest you want to create.

1. Open the Virmora `.dmg`.
2. Drag **Virmora** to **Applications**. For an update, quit Virmora first and replace the app when Finder prompts you.
3. Open Virmora from Applications. Choose **Create Machine**, select your guest ISO, and start the VM.

**Shared / NAT** works out of the box. NAT Network, Bridged, and Host-only may need [one-time administrator authorization](apps/desktop/README.md#build-a-self-contained-macos-dmg).

### Build a local installer

There is currently no public signed DMG. To build one for local testing, clone this repository on a Mac with Homebrew, Rust, a supported Node version, Python 3.9+, and the Xcode command-line tools. Run these commands from the repository root.

```sh
brew install qemu swtpm socket_vmnet
npm ci --prefix apps/desktop
python3 scripts/package-macos.py
```

Open the resulting `*-local.dmg` in `apps/desktop/src-tauri/target/release/bundle/dmg/` and follow the installation steps above. The script prints the package’s minimum macOS version, which depends on the bundled binaries. Public distribution requires Developer ID signing and Apple notarization. See the [desktop packaging guide](apps/desktop/README.md#build-a-self-contained-macos-dmg).

## Source build requirements

- macOS on Apple Silicon or Intel, Rust 1.89+, Node 20.19+ or 22.12+ for the desktop, Xcode command line tools.
- QEMU installed separately (`brew install qemu`), available on the application PATH.
- A Linux installer ISO matching the selected guest architecture, or a Linux kernel and compatible initramfs/root filesystem for direct boot. Source builds and the CLI use separately installed firmware.

## Run a Linux guest

```sh
cargo test --workspace --locked
cargo run -p virmora-cli -- --help
```

For an ISO VM, copy `examples/linux-iso-arm64.json`. For direct kernel boot, copy `examples/linux-arm64.json` and replace all paths with existing absolute paths. Use `architecture: "x86_64"` and `console=ttyS0` for an x86 guest. Native guests use macOS HVF; cross-architecture guests use slower TCG emulation.

```sh
cargo run -p virmora-cli -- create /absolute/path/to/config.json
cargo run -p virmora-cli -- list
cargo run -p virmora-cli -- run linux-arm64
```

`run` holds ownership of the VM. It prints the serial log path and a command such as `nc -U /…/run/linux-arm64/serial.sock` to open an interactive console in another terminal. Press Enter in the owner terminal to request shutdown. CLI process exit ends its owned VM. Start/stop across independent CLI invocations is deliberately not a daemon interface; the desktop offers persistent in-process controls.

The manager creates a sparse blank raw disk when the disk path is absent. A blank disk cannot boot a root filesystem by itself. Supply a prepared raw disk whose byte size matches `disk_size_gib` (whole GiB), or boot a suitable installer/initramfs and install Linux onto `/dev/vda`. The kernel must support virtio block/network and the appropriate serial device. Set the kernel root argument to the actual root partition (`/dev/vda`, `/dev/vda1`, etc.). Configure guest DHCP for NAT networking. qcow2 import is not implemented.

Configuration and runtime files default to `$HOME/.virmora`; set `VIRMORA_HOME` to choose a directory. An exclusive registry lock prevents multiple application owners against the same directory. Graceful stop has a five-second deadline followed by forced termination, which can interrupt guest writes. For filesystem verification, shut down cleanly inside the guest.

## Run the desktop app from source

```sh
cd apps/desktop
npm ci
npm run tauri dev
```

In **Create Machine**, choose a Linux ISO matching the guest architecture, select storage and hardware, create the machine, and start it. UEFI installer boot opens a separate native QEMU Cocoa window on macOS with keyboard and mouse support. Resize the window or use full screen to scale the guest image. Complete the installation onto the guest disk, shut down the guest, select **Boot installed disk**, and start again. Choose **Boot installer** while stopped to return to the selected ISO. Boot source changes are saved; Virmora does not infer that installation completed. A blank disk cannot boot until an operating system has been installed.

The same saved boot controls are available from the CLI:

```sh
cargo run -p virmora-cli -- boot MACHINE_ID installer
cargo run -p virmora-cli -- run MACHINE_ID
# After installing and shutting down the guest:
cargo run -p virmora-cli -- boot MACHINE_ID disk
cargo run -p virmora-cli -- run MACHINE_ID
```

Use `run MACHINE_ID --headless` to suppress the graphical window. Firmware installers may display only graphical output, so serial access alone may not show their interface. `boot MACHINE_ID auto` restores automatic selection: a configured kernel takes priority, otherwise a selected ISO is used, otherwise the disk is used. Explicit `disk` mode ignores retained ISO media, including a moved or deleted ISO.

The packaged desktop app uses its bundled UEFI firmware. Source builds and the CLI discover QEMU firmware in `/opt/homebrew/share/qemu`, `/usr/local/share/qemu`, or `/usr/share/qemu`. Optional absolute `firmware_code_path` and `firmware_vars_template_path` configuration fields override discovery. Linux ARM64 uses `edk2-aarch64-code.fd` with `edk2-arm-vars.fd`. Linux x86_64 uses `edk2-x86_64-code.fd` with `edk2-i386-vars.fd`. Virmora copies the variables template once to `<disk-path>.nvram.fd` and keeps it across restarts. Keep that file with the disk when backing up an installed machine.

Choose a network when creating a VM or under **Edit settings → Network** while the VM is stopped. The change applies at the next start.

- **Shared / NAT** uses QEMU user-mode networking and supplies guest DHCP.
- **NAT Network** uses macOS vmnet shared networking for peers on the same subnet and outbound internet.
- **Bridged** attaches the VM to the selected Mac network adapter. An active bridge-capable adapter and a LAN that accepts additional devices are required.
- **Host-only** uses macOS vmnet to connect the host and guests on a private network.
- **Internal Network** connects VMs owned by the same Virmora process without a host IP interface, gateway, or DHCP server. Assign static IP addresses on the same subnet in the guests.
- **Disconnected** removes the virtual NIC.

NAT Network, Bridged, and Host-only can use the macOS socket_vmnet services or a QEMU binary already authorized for vmnet. See the [desktop setup instructions](apps/desktop/README.md#authorize-the-macos-network-services). Use **Check network** in VM settings to verify the selected backend.

See `apps/desktop/README.md` for desktop controls and packaging. Tauri dependencies are isolated in their own Cargo workspace. The desktop and CLI use the same VM types and manager; use one at a time for a data directory.

## Windows 11 ARM64 preview

The Apple Silicon DMG includes the required TPM emulator and Windows ARM64 firmware. When running from source or using the CLI, install QEMU and run the following commands from the repository root.

```sh
brew install swtpm
bash scripts/setup-windows-firmware.sh
```

The script installs Debian's AAVMF firmware with a Microsoft-key UEFI variables template into your user Library. Virmora starts a software TPM 2.0 for each Windows VM and exposes an NVMe disk and a firmware framebuffer to Windows Setup.

In **Create Machine**, choose **Windows 11 ARM64 (preview)** and select the [official Microsoft ARM64 ISO](https://www.microsoft.com/en-us/software-download/windows11arm64) under **ISO Image**. The profile supports 2–8 CPUs and requires at least 4 GiB RAM and a 64 GiB virtual disk. Start the VM and press a key in its window if prompted to boot from the installer. To supply drivers for the virtual network adapter, attach an ISO containing signed Windows ARM64 VirtIO network drivers in **Windows driver ISO**. You can change or eject it in **Edit settings** while the VM is stopped.

After Windows installs, shut down the guest and choose **Boot installed disk** in Virmora. This detaches the installer on subsequent starts. Back up the disk together with its `.nvram.fd` and `.tpm-state` sidecars. Windows x64 and macOS guests remain future work.

The official ISO reached graphical Windows Setup and detected the NVMe disk on Apple M1. Full installation and the first boot from the installed disk are pending. See [validation](docs/validation.md#windows-11-arm64-setup) for the exact setup tested.

## Validation

Unit tests cover configuration, disk retention, registry reopen, command construction and lifecycle behavior. The QEMU crate also includes an explicitly ignored, opt-in real Linux boot test requiring QEMU and local guest artifacts. See its test source for environment variables and invocation. The opt-in test was run successfully on Apple Silicon with QEMU/HVF and a Debian ARM64 kernel. It checks the Linux kernel boot banner across two start/stop cycles and a host-written disk sentinel. It does not establish a booted userspace, guest DHCP, outbound connectivity, or writes surviving inside a guest filesystem. A separate end-to-end Python smoke test subsequently verified Alpine guest userspace, guest filesystem persistence across restart, DHCP and outbound NAT HTTP; see [recorded validation](docs/validation.md). Linux ARM64 and x86_64 launch configurations are implemented; Intel hardware remains unverified.

To repeat the kernel boot check with your own local assets:

```sh
VIRMORA_TEST_KERNEL=/absolute/path/to/Image \
VIRMORA_TEST_INITRD=/absolute/path/to/initrd \
cargo test -p vm-qemu --test boot -- --ignored --nocapture
```

The initrd is optional if the kernel and command line support your boot flow. Set `VIRMORA_TEST_ARCH` to `aarch64` or `x86_64` and `VIRMORA_TEST_CMDLINE` to override the default `rdinit=/bin/sh panic=-1` arguments. The test attaches a blank raw disk, so it is a kernel boot smoke check rather than a root filesystem installation test. CI runs formatting, Clippy, headless unit tests and the React production build; the guest integration test stays opt-in.

Detailed module boundaries, limitations and next milestones are in [docs/architecture.md](docs/architecture.md).

The repeatable guest filesystem/DHCP/NAT smoke test and recorded results are in [docs/validation.md](docs/validation.md).

New desktop virtual disks default to `~/vermora/virtual machines/<machine-id>/disk.raw`. The ISO Image file picker stores the selected installer path with the VM. Registry metadata and runtime files remain in `~/.virmora` so existing VM definitions stay available. With `VIRMORA_HOME`, new desktop disks use `<VIRMORA_HOME>/virtual machines`.

The opt-in `scripts/smoke-iso-install.py` test has completed a real ARM64 Linux ISO installation and disk-only reboot on Apple Silicon, including graphical keyboard input and persisted guest data. See [validation](docs/validation.md) for the exact ISO hash, artifacts and repeat command.

## Resource monitoring

VM details include live CPU, host memory and receive/send network charts, refreshed every two seconds. CPU and memory measure the QEMU host process. Network counters measure traffic at the guest’s virtual network adapter. Use `run MACHINE_ID --monitor` for JSON metrics in the CLI. See [monitoring behavior and real workload verification](docs/monitoring.md).
