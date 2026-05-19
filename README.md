# dcmfree

Reclaim disk space taken by orphan Windows container layers in
`C:\ProgramData\Microsoft\Windows\Containers\Layers`. Native GUI + CLI in
one binary.

These layers are full of NTFS reparse points and hardlinks that
`del` / `Remove-Item` cannot remove — they need `SeBackupPrivilege` +
`SeRestorePrivilege` and the HCS `HcsDestroyLayer` API. That's what
`dcmfree` does.

## Install

Grab `dcmfree.exe` from the
[Releases](https://github.com/robinduckett/dcmfree/releases) page, or
build from source:

```powershell
cargo build --release   # needs Rust 1.85+ (edition 2024)
```

## Usage

```powershell
dcmfree                 # open the GUI (default)
dcmfree list            # CLI: table of every layer
dcmfree info [--json]   # CLI: summary stats
dcmfree clean [opts]    # CLI: destroy orphan layers (admin required)
dcmfree /?              # help
```

`clean` options: `--dry-run`, `--yes`, `--min-age 7d`, `--layers-dir
<PATH>`.

Both the GUI and `clean` need an elevated process. The GUI shows a
confirm dialog and lets you cancel mid-destroy.

## Safety

- Cross-references Docker (in its current daemon mode) and skips layers
  Docker reports as in-use.
- CLI's `--min-age` (default 1 day) excludes recently-touched layers.
- Every Win32 handle is RAII-wrapped (`OwnedToken`, `HcsOperation`,
  `OwnedLocalAlloc`) — handles cannot leak even on panic.
- All destructive paths are gated by an explicit confirm.

## Why

Tools like `docker-ci-zap` target the legacy
`C:\ProgramData\Docker\windowsfilter` store, not the HCS-managed
`Microsoft\Windows\Containers\Layers` used by modern Docker, Windows
Sandbox, and Hyper-V isolated containers. As of mid-2026, no maintained
off-the-shelf tool cleaned this specific path.

## Build / test

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests use `tempfile::TempDir` only and never call into HCS,
so they're safe on any machine.

## License

[MIT](LICENSE).
