# Working on Virmora

Virmora is a macOS VM manager with a Rust backend and a Tauri 2 / React desktop app. It supports QEMU Linux guests with direct-kernel or UEFI boot. Apple Virtualization.framework, Windows guests, and macOS guests are planned.

## Code ownership

- Keep shared VM configuration and backend contracts in `crates/vm-core`.
- Put registry, settings, disk ownership, and lifecycle coordination in `crates/vm-manager`.
- Keep QEMU arguments, firmware, QMP, and process monitoring in `crates/vm-qemu`. Launch QEMU without a shell.
- Use `crates/virmora-cli` for the command-line client and `apps/desktop` for Tauri commands and React UI. The desktop Rust manifest is separate from the root workspace.

## Behavior to preserve

- Read existing VM configurations when adding fields. Use compatible defaults where appropriate.
- Keep configuration edits and deletion restricted to stopped machines. Keep graceful shutdown and force stop distinct, and keep disk retention selected by default.
- Treat the disk and its UEFI state as persistent VM data. Use a fresh temporary VM for destructive or boot tests. Keep installer images and local VM data out of Git.
- Show unavailable metrics as unavailable. Label host measurements separately from guest measurements.
- Describe networking and guest support based on what the backend actually implements and what has been tested.

## Verification

- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test --workspace --locked` for Rust changes.
- For desktop changes, run `npm ci --prefix apps/desktop`, `npm run build --prefix apps/desktop`, and `cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml`.
- See `docs/validation.md` and `docs/monitoring.md` for opt-in tests that need QEMU, local guest assets, or macOS process and socket access.
