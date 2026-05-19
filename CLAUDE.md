# CLAUDE.md — agent notes for working in this repo

## What this project is

A small Windows CLI in Rust that reclaims disk space from orphan container
layers in `C:\ProgramData\Microsoft\Windows\Containers\Layers` by calling the
HCS (`computestorage.dll`) `HcsDestroyLayer` API with backup/restore
privileges enabled.

The HCS layer store is shared between modern Docker Desktop (in
Windows-containers mode), Windows Sandbox, and Hyper-V isolated containers.
Manual `Remove-Item -Recurse -Force` silently fails on the reparse points
inside each layer, even when elevated — only the HCS API can actually destroy
them.

## Layout

- `src/main.rs` — binary entry; thin wrapper that calls `cli::run`.
- `src/lib.rs` — re-exports and the `DEFAULT_LAYERS_DIR` constant.
- `src/cli.rs` — clap-derived CLI definitions and command dispatch.
- `src/layers.rs` — pure-Rust filesystem enumeration + classification.
  Easy to unit-test; uses `walkdir` and `std::fs`.
- `src/docker.rs` — best-effort Docker query. Returns an empty set when the
  Docker CLI is missing.
- `src/privileges.rs` — Win32 privilege enablement (`SeBackupPrivilege`,
  `SeRestorePrivilege`) via `AdjustTokenPrivileges`.
- `src/hcs.rs` — single function wrapping `HcsDestroyLayer`.
- `src/errors.rs` — `thiserror`-based error type for the library.
- `src/format.rs` — pure formatters (bytes, human-readable durations) with
  thorough unit tests.
- `tests/cli.rs` — integration tests via `assert_cmd` against fake layer
  trees in `tempfile::TempDir`s. **They never invoke HcsDestroyLayer.**

## Windows-only

This crate is Windows-only. The HCS APIs only exist on Windows; there are
no cross-platform stubs.

## Adding a subcommand

1. Add a new variant + `Args` struct in `src/cli.rs`.
2. Implement a `cmd_xxx(args)` function next to the existing ones.
3. Wire it in `run()`.
4. Add a corresponding integration test in `tests/cli.rs` that uses a
   `tempfile::TempDir` of fake layers (see `make_fake_layers_dir`).

## Adding new HCS calls

If extending beyond `HcsDestroyLayer`, find the function in the
[`windows`](https://docs.rs/windows/) crate under
`Win32::Storage::HostComputeStorage`. Wrap it in `src/hcs.rs` with the same
pattern: PCWSTR for paths, map HRESULT failures to
`DcmFreeError::HcsDestroyFailed` (or add a new variant).

Privileges to enable: most HCS storage calls require both
`SeBackupPrivilege` and `SeRestorePrivilege`; `privileges::enable_backup_restore`
handles both.

## Tests on a real machine

Integration tests are safe to run on any machine: they operate exclusively
on `tempfile::TempDir`s and never call `HcsDestroyLayer`.

Manual end-to-end testing of `clean` requires:

1. Administrator PowerShell.
2. Real HCS layers under
   `C:\ProgramData\Microsoft\Windows\Containers\Layers`.
3. **Run `dcmfree clean --dry-run` first** to confirm the plan.

## Releasing

Tag-driven via `.github/workflows/release.yml`. Push a tag matching
`v[0-9]+.[0-9]+.[0-9]+` and CI builds `dcmfree.exe` on `windows-latest`,
attaching it to a draft release.

`Cargo.toml` `version` should be bumped before tagging.

## Conventions

- `cargo fmt` enforced in CI (`fmt --check`).
- `cargo clippy -- -D warnings` enforced in CI.
- Public functions have a brief doc comment above them.
- New error cases get a dedicated `DcmFreeError` variant rather than
  `anyhow::anyhow!`. CLI-level errors can still use `anyhow::Context` to add
  context.
- Tests favour `tempfile::TempDir` over hard-coded paths.
