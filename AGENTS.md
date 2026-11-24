# Working on Virmora

Virmora is a macOS VM manager with a Rust backend and a Tauri 2 / React desktop app. QEMU supports Linux guests with direct-kernel or UEFI boot. The Windows 11 ARM64 preview on Apple Silicon has reached Microsoft Windows Setup and detected the NVMe disk. Completing installation and booting an installed Windows guest still need verification. Windows x64, macOS guests, and Apple Virtualization.framework execution are planned.

## Code ownership

- Keep shared VM configuration and backend contracts in `crates/vm-core`.
- Put registry, settings, disk ownership, and lifecycle coordination in `crates/vm-manager`.
- Keep QEMU arguments, firmware, TPM lifecycle, networking, QMP, and process monitoring in `crates/vm-qemu`. Launch QEMU and its helpers without a shell.
- Use `crates/virmora-cli` for the command-line client and `apps/desktop` for Tauri commands and React UI. The desktop Rust manifest is separate from the root workspace.

## Behavior to preserve

- Read existing VM configurations when adding fields. Use compatible defaults where appropriate.
- Keep configuration edits and deletion restricted to stopped machines. Keep graceful shutdown and force stop distinct, and keep disk retention selected by default.
- Treat the disk, UEFI variables, and Windows TPM state as persistent VM data. Keep the Windows installer and installed-disk modes on the same virtual hardware. Use a fresh temporary VM for destructive or boot tests.
- Keep VM disks, firmware binaries, installer and driver ISOs, runtime state, logs, and credentials out of Git.
- Show unavailable metrics as unavailable. Label host measurements separately from guest measurements.
- Describe networking and guest support based on what the backend actually implements and what has been tested.

## Verification

- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test --workspace --locked` for Rust changes.
- For desktop changes, run `npm ci --prefix apps/desktop`, `npm run build --prefix apps/desktop`, and `cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml`.
- For host setup scripts, run `bash -n` and document dependencies, installation locations, and any required administrator access.
- See `docs/validation.md` and `docs/monitoring.md` for opt-in tests that need QEMU, local guest assets, or macOS process and socket access.
