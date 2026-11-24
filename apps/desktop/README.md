# Virmora desktop

Native Tauri 2 + React VM manager. Install Rust, Xcode command-line tools, Node 22 LTS, and QEMU (`brew install qemu`).

```sh
cd apps/desktop
npm ci
npm run tauri dev
```

`npm run build` checks TypeScript and builds the frontend; `cargo check --manifest-path src-tauri/Cargo.toml` checks the native application after the frontend has been built. `npm run tauri build -- --no-bundle` builds a local executable. Distribution signing and notarization are not configured.

VM files default to `/Users/username/vermora/virtual machines` (using your actual macOS username). The UI displays the active location, including any `VIRMORA_HOME` override. The registry is shared with the CLI. Only one manager may own a registry at a time. Close a CLI `run` session before opening the same registry in the UI. Closing the app terminates owned QEMU processes.

Inside **Create Machine**, use **Choose ISO…** beside **ISO Image** to select a Linux installer matching the chosen architecture. Give the machine a name, choose its storage folder and hardware, then create and start it. The installer opens in a dedicated native QEMU window with keyboard and mouse support. QEMU's packaged UEFI firmware is discovered automatically; each machine keeps its own persistent NVRAM beside its raw disk.

Complete the installation in the guest window, then shut down the guest. In Virmora select **Boot installed disk** and start the machine again. The boot source is saved explicitly; Virmora does not assume an installation has completed. Select **Boot installer** while stopped to return to the selected ISO. Disk boot uses the persistent disk even if the ISO has been moved. A blank disk will not boot until an operating system has been installed.

The machine details show the selected boot source and media path. The serial console is a secondary troubleshooting panel; graphical installers use the separate VM display. Existing CLI-created direct-kernel machines keep their serial boot workflow. Closing Virmora asks each running guest to shut down, then terminates any process that remains after the backend timeout.

Existing CLI-created machines appear automatically. On Apple Silicon, select **Windows 11 ARM64 (preview)** in **Create Machine**. See the [Windows host setup and ISO instructions](../../README.md#windows-11-arm64-preview). Windows x64, macOS guests, and Apple Virtualization.framework execution remain future milestones.

The Network section is available in Create Machine and Edit Settings. Stop the VM before changing its mode. Choose a Mac adapter for Bridged. NAT Network, Bridged, and Host-only require vmnet authorization for QEMU. For Internal Network, assign static addresses on the same subnet to guests running in the same Virmora session. Older configurations without a network mode default to Shared / NAT.

Resource monitoring samples the running QEMU process every two seconds and charts the latest 60 samples. CPU is host process CPU cost (100% equals one host core); host memory is the QEMU process footprint, not the guest's reported used memory. Network readings describe the host process's TCP/UDP NAT traffic, not every frame on the guest's virtual NIC. Platforms or network configurations that cannot provide a measurement display Sampling or Unavailable rather than invented zeroes. Rates require successive samples. Counters and charts reset when the VM process restarts; stopped machines display no stale readings. Measurement failures appear inside the monitoring panel and retry automatically. Sampling is sequential, and results from a previously selected machine are discarded.

Use **Edit settings** on a stopped machine to change its name, CPU cores, memory, network mode, or installer ISO. Save after choosing or ejecting an ISO. Ejecting the active installer switches the boot source to the installed disk.

**Shut down** asks the guest to power off. **Restart** waits up to 30 seconds for the guest to exit before starting it again. If the guest ignores the request, you can shut it down from inside the guest or confirm **Force stop**.

**Delete machine** defaults to keeping the disk and its UEFI state. The recovery configuration is saved beside the disk as `<disk-path>.virmora-recovery`. Restore the registration with `virmora create /absolute/path/to/disk.raw.virmora-recovery`. The delete confirmation can also remove the disk and its UEFI state. The installer ISO is kept.

**Logs and diagnostics** shows the most recent startup error and the tails of the QEMU and serial logs.

### Authorize the macOS network services

Install `socket_vmnet` with Homebrew, then run the setup script from the repository root in Terminal. Change `en0` to the adapter selected for Bridged mode.

```sh
brew install socket_vmnet
bash scripts/setup-macos-networking.sh en0
```

The setup asks you to authenticate as a Mac administrator. It installs root-owned copies of the packaged helper and client, then loads separate launchd services for NAT Network, Host-only, and the selected Bridged adapter. The socket directory grants access to the account that ran setup. Virmora and QEMU continue to run as that account. The shared and host-only networks use `192.168.105.0/24` and `192.168.106.0/24`, respectively.

In **Edit Settings → Network**, select the mode and use **Check network**. The check opens the selected backend with a paused QEMU instance and closes it immediately. A ready result confirms that the VM can open the backend. Guest connectivity also depends on guest DHCP, the host adapter, and the LAN.

To remove a service, unload its label from launchd and remove its corresponding plist from `/Library/LaunchDaemons`. The labels are `dev.virmora.network.shared`, `dev.virmora.network.host`, and `dev.virmora.network.bridged-en0` for the default adapter. Stop VMs using these services before unloading them.
