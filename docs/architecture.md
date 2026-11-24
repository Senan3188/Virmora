# Virmora architecture

Virmora is a local, single-owner VM application. Rust owns configuration, storage and processes; React renders status and forwards typed Tauri commands. The Linux MVP supports direct kernel boot and UEFI ISO installation followed by installed-disk boot. The Windows 11 ARM64 preview adds a QEMU installer profile on Apple Silicon.

## Modules

- `vm-core`: serializable VM definitions, validation, lifecycle states and backend trait. No UI or hypervisor dependency.
- `vm-manager`: persistent registry, sparse raw disk provisioning, prepared raw disk import and lifecycle orchestration. Metadata is written using a temporary file, fsync and rename. A process-lifetime advisory file lock enforces one registry owner. Disk import rejects symlinks and prevents attaching the same disk twice, including hard-link aliases. Backend state is queried rather than trusting persisted running flags.
- `vm-qemu`: child-process ownership, architecture-specific device configuration, user-mode NAT, direct kernel and UEFI ISO/disk boot, persistent per-VM NVRAM, native Cocoa display, serial socket/log and QMP stop. Matching macOS architectures use HVF; cross-architecture guests use TCG.
- `vm-apple`: explicit unsupported adapter reserved for Virtualization.framework integration. It never reports a VM running without creating one.
- `virmora-cli`: minimal executable reference client. The `run` process owns the VM until Enter or EOF.
- `apps/desktop`: Tauri 2 commands and React UI. Desktop dependencies are isolated from the core Cargo workspace so headless tests do not require WebKit or a signing setup.

## First milestone contract

The user provides either an architecture-compatible Linux installer ISO or a compatible Linux kernel and optional initramfs. A raw disk is retained across stop and restart. Creating a blank disk does not install Linux: use an installer/initramfs to format and populate it, or supply a prepared raw root disk whose size matches the configured whole-GiB capacity. Existing disks are never truncated. For a prepared root filesystem, pass the appropriate `root=/dev/vda… rw` kernel arguments. The kernel needs virtio block/network and the relevant serial driver, either built in or in the initramfs. An ARM guest generally uses `console=ttyAMA0`; x86 uses `console=ttyS0`.

QEMU user networking provides DHCP and outbound NAT. On macOS, NAT Network, Bridged, and Host-only use the matching socket_vmnet service when present and otherwise try QEMU's native vmnet backend. The services provide vmnet access while keeping the desktop and QEMU unprivileged. Bridged uses the saved Mac adapter. Internal Network connects QEMU Ethernet streams through a Unix socket in a private, owner-only directory. It forwards frames between guests in one Virmora process and supplies no IP interface, DHCP, or internet gateway. Disconnected omits the NIC. Serial output is saved locally; the Unix serial socket supports interactive terminal access. UEFI guests have a virtio GPU and USB keyboard/tablet, with a separate native QEMU Cocoa guest window on macOS. The graphical display is not embedded in the Tauri UI; direct-kernel guests remain serial-first. CLI `run ID --headless` disables the graphical window.

Boot selection is persisted as `boot_mode`: `installer` attaches the selected ISO read-only, `disk` omits it, and `auto` prioritizes a nonempty direct kernel path, then a selected ISO, then the disk. The manager only permits changing this setting while stopped and atomically writes the configuration without altering the disk. Desktop **Boot installer** / **Boot installed disk** controls and CLI `boot ID installer|disk|auto` share this operation. Disk boot ignores retained ISO references, including missing media. Installation completion is a user decision, not inferred from disk contents.

Firmware boot uses QEMU's EDK2 code and variables template. Discovery checks `/opt/homebrew/share/qemu`, `/usr/local/share/qemu` and `/usr/share/qemu`; `firmware_code_path` and `firmware_vars_template_path` accept explicit absolute file paths. ARM64 selects `edk2-aarch64-code.fd` / `edk2-arm-vars.fd`; x86_64 selects `edk2-x86_64-code.fd` / `edk2-i386-vars.fd`. The template initializes `<disk-path>.nvram.fd` once, preserving guest boot entries and firmware settings across restarts. Existing variables files are never reset automatically. Firmware is provided by the user's QEMU installation and is not bundled.

Stop requests ACPI poweroff over QMP and allows a bounded shutdown period; a guest that does not respond is forcibly terminated. Shut down from inside the guest before stopping when testing filesystem persistence. Process ownership prevents normal application exit from leaving an unmanaged VM. Abrupt host termination is not a transactional guest shutdown. The registry is configuration persistence, not a recovery daemon. The registry lock enforces a single application owner per data directory; there is no separate privileged service or daemon.

## Native Apple adapter

Implement a separate macOS adapter using generated Objective-C bindings and an owned dispatch queue. Configure `VZLinuxBootLoader`, `VZVirtioBlockDeviceConfiguration`, `VZNATNetworkDeviceAttachment` and serial file-handle attachments, validate before start, and map asynchronous completions and delegate callbacks into backend state. The app must be signed with `com.apple.security.virtualization`. The native adapter remains a separate future milestone; the current tested execution path uses QEMU.

Apple's framework virtualizes compatible host architectures. macOS guests require an Apple-supported restore/install workflow and host hardware. The Windows 11 ARM64 QEMU profile uses AAVMF, a framebuffer, NVMe storage, USB installer media, and a per-VM swtpm process with persistent TPM state. An optional second ISO supplies ARM64 guest drivers.

## Next milestones

1. Run the successful Apple Silicon/HVF guest userspace, DHCP, outbound HTTP and guest filesystem persistence smoke checks on Intel hardware and more distro profiles. See `docs/validation.md` for the separate Rust kernel-banner and Python guest tests.
2. Add the native Apple Linux adapter with signed-host integration tests.
3. Expand UEFI installation profiles and guest compatibility, add an embedded graphical console, and validate installed-disk workflows on Intel hardware. Linux ARM64 and x86_64 command profiles are implemented; Intel hardware is not yet tested.
4. Add macOS restore/install and Windows x64 profiles, then snapshots, guest tools and sharing.

## References

- [Apple: Virtualize Linux on a Mac](https://developer.apple.com/documentation/virtualization/virtualize-linux-on-a-mac)
- [Apple: virtualization entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.virtualization)
- [QEMU ARM virt machine](https://www.qemu.org/docs/master/system/arm/virt.html)
- [QEMU invocation and networking](https://www.qemu.org/docs/master/system/invocation.html)
