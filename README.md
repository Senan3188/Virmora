# Virmora

Rust VM manager with QEMU, a Tauri 2 / React desktop client, and an explicit extension point for Apple Virtualization.framework. The Linux MVP implements UEFI ISO installation, boot from an installed disk, direct kernel boot, retained raw disks and NVRAM, outbound NAT, graphical guest windows, serial console and start/stop lifecycle. Windows/macOS guests and the native Apple backend are roadmap work.

## Requirements

- macOS on Apple Silicon or Intel, Rust 1.89+, Node 20.19+ or 22.12+ for the desktop, Xcode command line tools.
- QEMU installed separately (`brew install qemu`), available on the application PATH.
- A Linux installer ISO matching the selected guest architecture, or a Linux kernel and compatible initramfs/root filesystem for direct boot. Guest images and firmware are not bundled.

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

## Desktop

```sh
cd apps/desktop
npm ci
npm run tauri dev
```

In **Create Machine**, choose a Linux ISO matching the guest architecture, select storage and hardware, create the machine, and start it. UEFI installer boot opens a separate native QEMU Cocoa window on macOS with keyboard and mouse support. Complete the installation onto the guest disk, shut down the guest, select **Boot installed disk**, and start again. Choose **Boot installer** while stopped to return to the selected ISO. Boot source changes are saved; Virmora does not infer that installation completed. A blank disk cannot boot until an operating system has been installed.

The same saved boot controls are available from the CLI:

```sh
cargo run -p virmora-cli -- boot MACHINE_ID installer
cargo run -p virmora-cli -- run MACHINE_ID
# After installing and shutting down the guest:
cargo run -p virmora-cli -- boot MACHINE_ID disk
cargo run -p virmora-cli -- run MACHINE_ID
```

Use `run MACHINE_ID --headless` to suppress the graphical window. Firmware installers may display only graphical output, so serial access alone may not show their interface. `boot MACHINE_ID auto` restores automatic selection: a configured kernel takes priority, otherwise a selected ISO is used, otherwise the disk is used. Explicit `disk` mode ignores retained ISO media, including a moved or deleted ISO.

UEFI firmware is discovered from the separately installed QEMU package in `/opt/homebrew/share/qemu`, `/usr/local/share/qemu`, or `/usr/share/qemu`. Optional absolute `firmware_code_path` and `firmware_vars_template_path` configuration fields override discovery. ARM64 uses `edk2-aarch64-code.fd` with `edk2-arm-vars.fd`; x86_64 uses `edk2-x86_64-code.fd` with `edk2-i386-vars.fd`. A VM's variables template is copied once to `<disk-path>.nvram.fd` (for example `disk.raw.nvram.fd`); subsequent boots retain its UEFI settings and boot entries. Keep this file together with the disk when backing up an installed machine. Firmware is not redistributed by Virmora.

Shared/NAT and Disconnected networking are implemented. The other network choices are planned and return an explicit unsupported-mode error on start.

See `apps/desktop/README.md` for desktop controls and packaging. Tauri dependencies are isolated in their own Cargo workspace. The desktop and CLI use the same VM types and manager; use one at a time for a data directory.

## Validation

Unit tests cover configuration, disk retention, registry reopen, command construction and lifecycle behavior. The QEMU crate also includes an explicitly ignored, opt-in real Linux boot test requiring QEMU and local guest artifacts. See its test source for environment variables and invocation. The opt-in test was run successfully on Apple Silicon with QEMU/HVF and a Debian ARM64 kernel. It checks the Linux kernel boot banner across two start/stop cycles and a host-written disk sentinel. It does not establish a booted userspace, guest DHCP, outbound connectivity, or writes surviving inside a guest filesystem. A separate end-to-end Python smoke test subsequently verified Alpine guest userspace, guest filesystem persistence across restart, DHCP and outbound NAT HTTP; see [recorded validation](docs/validation.md). Linux ARM64 and x86_64 launch configurations are implemented; Intel hardware remains unverified.

To repeat the kernel boot check with your own local assets:

```sh
VIRMORA_TEST_KERNEL=/absolute/path/to/Image \
VIRMORA_TEST_INITRD=/absolute/path/to/initrd \
cargo test -p vm-qemu --test boot -- --ignored --nocapture
```

The initrd is optional if the kernel and command line support your boot flow. Set `VIRMORA_TEST_ARCH` to `aarch64` or `x86_64` and `VIRMORA_TEST_CMDLINE` to override the default `rdinit=/bin/sh panic=-1` arguments. The test attaches a blank raw disk, so it is a kernel boot smoke check rather than a root filesystem installation test. CI runs formatting, Clippy, headless unit tests and the React production build; the guest integration test stays opt-in.

Detailed module boundaries, limitations and next milestones are in [docs/architecture.md](docs/architecture.md). QEMU is an external runtime and is not redistributed by this repository.

The repeatable guest filesystem/DHCP/NAT smoke test and recorded results are in [docs/validation.md](docs/validation.md).

New desktop virtual disks default to `~/vermora/virtual machines/<machine-id>/disk.raw`. The ISO Image file picker stores the selected installer path with the VM. Registry metadata and runtime files remain in `~/.virmora` so existing VM definitions stay available. With `VIRMORA_HOME`, new desktop disks use `<VIRMORA_HOME>/virtual machines`.

The opt-in `scripts/smoke-iso-install.py` test has completed a real ARM64 Linux ISO installation and disk-only reboot on Apple Silicon, including graphical keyboard input and persisted guest data. See [validation](docs/validation.md) for the exact ISO hash, artifacts and repeat command.

## Resource monitoring

VM details include live CPU, host memory and receive/send network charts, refreshed every two seconds. These measure the QEMU host process; guest-internal memory usage is not inferred from its RAM allocation. Use `run MACHINE_ID --monitor` for JSON metrics in the CLI. See [monitoring behavior and real workload verification](docs/monitoring.md).
