# dcmfree

**Disk Cleanup for Microsoft Containers — Free space stuck in orphan layers.**

A small Windows CLI that reclaims disk space taken by orphan container layers
in `C:\ProgramData\Microsoft\Windows\Containers\Layers`. These accumulate from
Windows-containers-mode Docker, Windows Sandbox sessions that didn't clean up,
and old Hyper-V container activity. They're full of NTFS reparse points and
hardlinks that ordinary `del` / `Remove-Item` cannot delete — even from an
elevated prompt — because they require `SeBackupPrivilege` /
`SeRestorePrivilege` and the proper HCS API.

`dcmfree` calls `HcsDestroyLayer` from `computestorage.dll` after enabling the
right privileges. That's the supported way to remove these.

## Install

### From a release

Download the latest `dcmfree.exe` from the
[Releases](https://github.com/robinduckett/dcmfree/releases) page, drop it
anywhere on your `PATH`.

### From source

```powershell
git clone https://github.com/robinduckett/dcmfree
cd dcmfree
cargo build --release
# Binary is at target\release\dcmfree.exe
```

Requires Rust 1.75 or later.

## Usage

All commands accept `--layers-dir <PATH>` to override the default location.

### List layers

```powershell
dcmfree list
```

Shows every direct subfolder of the layers directory with size, age, orphan
status (cross-referenced against `docker images` and `docker ps -a` if Docker
is on `PATH`), and the layer ID.

### Summary

```powershell
dcmfree info
dcmfree info --json
```

Reports total layer count, orphan count, total size, and reclaimable size.

### Clean (destructive)

```powershell
# Show what would be destroyed without doing anything:
dcmfree clean --dry-run

# Destroy orphan layers older than 7 days, no prompt:
dcmfree clean --min-age 7d --yes
```

`clean` requires:

- **Administrator** (HCS APIs reject unelevated callers).
- `SeBackupPrivilege` / `SeRestorePrivilege` (dcmfree enables these
  automatically — they're present on admin tokens but disabled by default).

Default `--min-age` is 1 day, which keeps `dcmfree` from racing against an
in-progress container operation. Pass `--min-age 0` to disable the age guard.

## Safety

- `dcmfree` never destroys a layer that Docker (in the current daemon mode)
  reports as in-use, when the Docker CLI is on `PATH`.
- If Docker isn't available, every layer is treated as potentially orphan and
  the user is warned via `--verbose`.
- `clean` always prints the destruction plan first; without `--yes` it asks
  for confirmation on a TTY, and aborts cleanly on non-TTY stdin.
- Layers newer than `--min-age` are excluded by default.
- `clean` does nothing on non-Windows platforms (the HCS APIs are
  Windows-only).

## Why this tool exists

Microsoft's `docker-ci-zap` (and the [arcanericky fork](https://github.com/arcanericky/dockercizap))
target `C:\ProgramData\Docker\windowsfilter` — the *legacy* Docker Engine for
Windows layer store. They don't touch the HCS-managed
`Microsoft\Windows\Containers\Layers` location used by modern Docker Desktop,
Windows Sandbox, and Hyper-V isolated containers.

PowerShell scripts that just `Remove-Item -Recurse -Force` silently fail on
the layer trees because each one contains junctions and reparse points that
require backup/restore privilege to traverse. `Remove-Item` reports "Remove
Directory" lines as if working, then leaves most of the data behind.

`dcmfree` exists because, as of mid-2026, no maintained off-the-shelf tool
targeted this specific cleanup path.

## How it works

1. **Enumerate** subdirectories of the layers folder, compute size + mtime.
2. **Classify** each layer as orphan or in-use by cross-referencing
   `docker images --no-trunc -q` and `docker ps -a -q --no-trunc` with
   `docker inspect` `GraphDriver.Data.dir`.
3. **Filter** to orphans older than `--min-age`.
4. **Acquire** `SeBackupPrivilege` + `SeRestorePrivilege` via
   `AdjustTokenPrivileges`.
5. **Destroy** each target via `HcsDestroyLayer` from `computestorage.dll`.

## Limitations

- Windows-only. The HCS APIs are Windows-only and so is this tool; it does
  not build on other platforms.
- Cannot reclaim a layer that is genuinely being held open by a running
  container or service; in that case the HCS call fails with a busy/locked
  HRESULT, which is reported verbatim.
- Trusts Docker's `GraphDriver.Data.dir` field when present. Old Docker
  versions may report different shapes; the JSON parser is permissive and
  skips records it can't interpret.

## Building / testing

```powershell
cargo build
cargo test                # unit + integration tests against fake layer trees
cargo clippy -- -D warnings
cargo fmt --check
```

The integration tests in `tests/cli.rs` invoke the built binary against
temporary directories of fake layers — they never call `HcsDestroyLayer`, so
they're safe to run on any machine.

## Releasing

Push a `v*` tag (e.g. `v0.1.0`). The
[`release.yml`](.github/workflows/release.yml) workflow builds a signed
`dcmfree.exe` on a Windows runner and attaches it to a GitHub Release.

## License

[MIT](LICENSE).
