# MVP verification — 23 November 2026

Verified on this Apple Silicon macOS host with Rust 1.98 and QEMU 11.1.2. QEMU was installed through Homebrew. No guest images are included in the repository.

## Results

- Core workspace unit tests: 29 passed, expanded for UEFI, NVRAM, boot modes and monitoring. Covers persisted configuration, exclusive ownership, disk preservation/import, path alias rejection, QEMU arguments and explicit forced-stop state.
- Rust Clippy with warnings denied: passed.
- React TypeScript + Vite production build: passed.
- Native Tauri application `cargo check`: passed; GUI interaction and signed packaging have not been exercised.
- Opt-in Rust integration test: Debian ARM64 kernel boot banner observed on two starts with HVF, host disk sentinel retained.
- `scripts/smoke-linux.py`: Alpine ARM64 shell reached, guest FAT filesystem created on a fresh raw disk, file written and synced, VM stopped/restarted, file remounted and verified. Guest DHCP obtained `10.0.2.15`, DNS resolved `example.com`, and outbound HTTP fetched nonempty content through QEMU user networking.

The Python test creates its own fresh 1 GiB sparse disk under `/tmp/vm-smoke-*`. It never formats a disk supplied by the user. Its logs and disk are retained for inspection; the successful run is `/tmp/vm-smoke-rg0vlwfh` on this host. The filesystem is unmounted before VM stop. This checks Linux data persistence and networking, not general distro installation or GUI boot.

## Repeat the guest test

Install QEMU, build the CLI, and download the Alpine ARM64 virtualization kernel and initramfs from the [official Alpine distribution](https://dl-cdn.alpinelinux.org/alpine/v3.23/releases/aarch64/netboot/):

```sh
mkdir -p /tmp/virmora-boot-assets
curl -fL https://dl-cdn.alpinelinux.org/alpine/v3.23/releases/aarch64/netboot/vmlinuz-virt -o /tmp/virmora-boot-assets/alpine-kernel
curl -fL https://dl-cdn.alpinelinux.org/alpine/v3.23/releases/aarch64/netboot/initramfs-virt -o /tmp/virmora-boot-assets/alpine-initramfs
cargo build -p virmora-cli
python3 scripts/smoke-linux.py /tmp/virmora-boot-assets/alpine-kernel /tmp/virmora-boot-assets/alpine-initramfs
```

The downloaded artifacts used for this run have SHA-256 hashes:

```text
4ac63d06e6cf5820905ffa7741292cfbe90fb0cf1edba4c42e871f6daf699437  alpine-kernel
a497b544282f1e0558e03529ce99d515aabdf3cea06011fbbcc48604f482cacc  alpine-initramfs
```

These release-directory URLs can change when Alpine publishes an update; the hashes record the exact assets tested, not a claim that future downloads match. The test requires QEMU, HVF access, Unix socket permissions, Python 3 and outbound guest internet access. Run it outside a sandbox that denies socket binding. The Rust test accepts `VIRMORA_TEST_KERNEL`, `VIRMORA_TEST_INITRD`, `VIRMORA_TEST_ARCH` and `VIRMORA_TEST_CMDLINE`:

```sh
VIRMORA_TEST_KERNEL=/absolute/kernel VIRMORA_TEST_INITRD=/absolute/initramfs cargo test -p vm-qemu --test boot -- --ignored --nocapture
```

## Remaining verification

Intel macOS and x86 guests have not been exercised on hardware. Native Apple virtualization, macOS guest installation, signed distributable packaging, and power-loss recovery are outside this milestone. Graphical ARM64 Linux installation and installed-disk boot were exercised as described below. Desktop compile and launch results should be checked separately from the Rust headless suite.


## UEFI ISO installation and installed-disk boot

The full installation test passed on this Apple Silicon host with QEMU 11.1.2 and HVF. It used the official `alpine-virt-3.23.8-aarch64.iso`, verified against the distribution's SHA-256:

```text
a7a629b626a7bb9e72590b5ff37261aea99cf3fa5ddd395ca4681f15465a6f94
```

The test booted the unmodified ISO through EDK2 UEFI, captured the guest framebuffer, sent USB keyboard events to log into the graphical tty, and confirmed an absolute USB pointing device was present. It used Alpine's `setup-disk` to install Linux, GRUB and an EFI partition onto a new 4 GiB raw virtual disk. It shut the guest down, persisted Disk boot mode, changed the test configuration's ISO reference to a nonexistent path, and started again. The installed guest booted from `/dev/vda2` as an ext4 root filesystem and verified `/etc/virmora-install-proof`. QMP block device inspection confirmed no ISO was attached. Both guest shutdowns completed cleanly.

Successful test artifacts are retained on this host at `/tmp/virmora-iso-ez_heea5`: `installer.png`, `graphical-keyboard-login.png`, `installation-complete.png`, `installed-disk.png`, serial and owner logs, disk, NVRAM and `disk-block-devices.json`. The test uses a known password only inside its isolated temporary guest and exposes no SSH or VNC service.

To repeat:

```sh
curl -fL https://dl-cdn.alpinelinux.org/alpine/v3.23/releases/aarch64/alpine-virt-3.23.8-aarch64.iso -o /tmp/alpine-virt-3.23.8-aarch64.iso
cargo build -p virmora-cli
python3 scripts/smoke-iso-install.py /tmp/alpine-virt-3.23.8-aarch64.iso --sha256 a7a629b626a7bb9e72590b5ff37261aea99cf3fa5ddd395ca4681f15465a6f94
```

The test creates its own new `/tmp/virmora-iso-*` directory and only formats that guest disk. It retains artifacts rather than deleting them. Package installation requires guest outbound internet access. Native Cocoa display and Unix sockets require an unsandboxed macOS session. It is opt-in rather than part of headless CI. This validates ARM64 Linux, not Windows/macOS guests or native Intel hardware.

## Windows 11 ARM64 Setup

The Microsoft Windows 11 26H2 ARM64 English image (build 26300.9457) reached graphical Windows Setup on macOS 26.6.2 with an Apple M1, QEMU 11.1.2, Debian AAVMF 2025.02, and swtpm 0.10.2. The VM used 2 vCPUs, 4 GiB RAM, a 64 GiB sparse raw disk, and the QEMU framebuffer. Windows PE's `diskpart list disk` showed the NVMe system disk online with 64 GB free.

QEMU first stalled during the Windows boot-manager handoff with GICv3. GICv2 with `hvf,kernel-irqchip=off` reached Setup and is now the Windows profile default. QMP confirmed the TPM device, and a separate stop/start check verified that disk mode detaches the installer and retains the TPM state. Completing Windows Setup, validating the guest network driver, and booting a finished Windows installation remain pending.

Firmware boots use fixed PCI addresses and explicit disk/CD boot priorities so ejecting the installer does not move the installed disk. Per-VM NVRAM is preserved; malformed or symlinked NVRAM is rejected rather than overwritten. QEMU firmware is supplied by the separately installed QEMU package.

A separate x86_64 UEFI smoke test also passed on this ARM host using TCG: the guest rendered the firmware shell, QMP reported Running, and PCI inspection found exactly one graphics adapter. Artifacts: `/tmp/virmora-x86-9sskk9sd/firmware.png`. This is firmware validation under emulation, not an x86 Linux installation or native Intel hardware test.

## Resource monitoring

The real monitoring workload test passed on an ARM64 Linux VM with HVF. CPU readings rose to 98.9% under a busy loop, QEMU physical footprint grew after a 64 MiB guest allocation, and a local 16 MiB HTTP transfer produced live receive/send counters and rates. Verified samples: `/tmp/vm-monitor-j9e_674i/verified-samples.json`. Monitoring measures the QEMU host process, not guest-internal used RAM. See [monitoring details](monitoring.md) for scope, limitations and repeat commands.

## VM network modes

An opt-in test booted two ARM64 Alpine guests on one Internal Network, assigned static IP addresses, and verified guest-to-guest ping with no IPv4 default route. Run it with the same Alpine virt kernel and initramfs used for the direct boot smoke test:

```sh
VIRMORA_TEST_KERNEL=/absolute/path/to/vmlinuz-virt \
VIRMORA_TEST_INITRD=/absolute/path/to/initramfs-virt \
cargo test -p vm-qemu --test internal_network -- --ignored
```

The bundled Homebrew QEMU on the test Mac listed all three macOS vmnet backends, but macOS denied creation of an unprivileged shared interface. NAT Network, Host-only, and Bridged still need an end-to-end test with a QEMU build authorized to use vmnet.

The socket_vmnet client (Homebrew 1.2.2) passed an unprivileged transport check on macOS using a disposable Unix socket. QEMU accepted the inherited socket and completed the QMP startup and shutdown exchange. This check did not start the privileged vmnet services.

After authorizing the services, use **Check network** for each mode and then verify guest connectivity:

- **NAT Network**. Boot two guests on NAT Network, confirm both get DHCP addresses on the shared subnet, ping one guest from the other, and fetch an HTTPS page from a guest.
- **Host-only**. Boot a guest on Host-only and ping its DHCP address from the Mac.
- **Bridged**. Choose the active physical adapter in Edit Settings, boot a guest, and check that it receives an address from the LAN's DHCP server. Ping a known LAN peer that accepts ICMP.
