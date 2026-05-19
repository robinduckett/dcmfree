//! `dcmfree` — reclaim disk space taken by orphan Windows container layers.
//!
//! Modern Docker Desktop (Linux mode), Windows Sandbox, and Windows containers
//! all share the Host Compute Service (HCS) storage at
//! `C:\ProgramData\Microsoft\Windows\Containers\Layers`. When containers exit
//! abnormally, the daemon is reinstalled, or Docker switches modes, layers
//! often remain on disk with reparse points and hardlinks that ordinary
//! `del`/`Remove-Item` cannot clean up — they silently fail because they
//! lack `SeBackupPrivilege`/`SeRestorePrivilege`.
//!
//! `dcmfree` calls the proper HCS API (`HcsDestroyLayer`) after acquiring
//! the required privileges, which is the supported way to remove these layers.
//!
//! This crate is **Windows-only**.

#![cfg(windows)]

pub mod cli;
pub mod docker;
pub mod errors;
pub mod format;
pub mod gui;
pub mod hcs;
pub mod layers;
pub mod privileges;
pub mod util;

/// Default path for HCS-managed container layers on Windows.
pub const DEFAULT_LAYERS_DIR: &str = r"C:\ProgramData\Microsoft\Windows\Containers\Layers";
