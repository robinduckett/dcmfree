# CLAUDE.md

## Layout

- `main.rs` — hybrid dispatch: no args → GUI, `/?` → help, else CLI.
  `windows_subsystem = "windows"` in release; `AttachConsole` re-attaches
  to a parent shell so CLI subcommands print. `MessageBoxW` fallback
  surfaces GUI-launch errors when no console is attached.
- `lib.rs` — modules + `DEFAULT_LAYERS_DIR` constant.
- `cli.rs` — clap subcommands: `list`, `info`, `clean`, `gui`.
- `gui.rs` — `native-windows-gui` Win32 GUI. Worker-thread scan +
  destroy, bridged to the UI via `nwg::Notice` + `mpsc::channel`.
  `Arc<AtomicBool>` (Relaxed) for cancellation.
- `layers.rs` — `stat_layer` walks one layer dir: size, file count,
  `CreationTime` (drives `age`), modified, accessed. Pure Rust;
  `TempDir`-testable.
- `docker.rs` — best-effort `docker images` + `docker inspect` for
  in-use layer paths. Returns empty set if docker isn't on `PATH`.
- `privileges.rs` — `SeBackupPrivilege` / `SeRestorePrivilege` via
  `AdjustTokenPrivileges`. `OwnedToken` RAII wraps the process token.
- `hcs.rs` — HCS API. `HcsApi` (zero-sized) exposes `destroy_layer` +
  `enumerate_compute_systems`. `HcsOperation` RAII wraps
  `HCS_OPERATION`. `OwnedLocalAlloc` RAII wraps the `LocalAlloc` buffer
  returned by `HcsWaitForOperationResult`. Functions live in
  `windows::Win32::System::HostComputeSystem` (not `HostComputeStorage`
  — that namespace doesn't exist in `windows-rs 0.60`).
- `errors.rs` — `thiserror`-based `DcmFreeError`.
- `format.rs` — `bytes` + `age` formatters.
- `util.rs` — `wide_nul` (NUL-terminated UTF-16 encoder).
- `tests/cli.rs` — `assert_cmd` integration tests against `TempDir`
  fake layers. Never invoke `HcsDestroyLayer`.

## Conventions

- Rust 2024, MSRV 1.85. `&raw mut` / `&raw const` for raw-pointer
  borrows; let-chains allowed.
- Every Win32 handle has an RAII wrapper. Manual `CloseHandle` /
  `LocalFree` / `HcsCloseOperation` are never used directly.
- Every `unsafe { ... }` block has a `// SAFETY:` comment above it.
- New error cases get a `DcmFreeError` variant; `anyhow::Context` is
  fine at CLI-level boundaries.
- Imports: `std` → external crates → crate-local, blank line between
  blocks. Hoist to top of file; no inline `use`.
- Tests use `tempfile::TempDir` and never mutate process-wide state
  (env vars, cwd) — `cargo test` runs them in parallel.

## Releasing

Bump `Cargo.toml` `version`, push a `v[0-9]+.[0-9]+.[0-9]+` tag.
`release.yml` builds + tests on `windows-latest` and publishes the
zipped `dcmfree.exe` + SHA-256.

## CI gates

```
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The codebase also stays clean under `clippy::pedantic` +
`clippy::nursery`, modulo the four standard opt-outs
(`missing_errors_doc`, `missing_panics_doc`, `multiple_crate_versions`,
`module_name_repetitions`).
