//! Acquire `SeBackupPrivilege` and `SeRestorePrivilege` for the current process.
//!
//! HCS APIs need these to traverse and delete the container layer reparse
//! points and hardlinks. The privileges are *present* on Administrator
//! tokens but not *enabled* by default — `AdjustTokenPrivileges` flips
//! them on.
//!
//! All Win32 handles allocated here are wrapped in [`OwnedToken`] so they
//! close on drop, even on panic. Manual `CloseHandle` is not used anywhere
//! in this module.

use std::io;

use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ACCESS_MASK, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::IsUserAnAdmin;
use windows::core::PCWSTR;

use crate::errors::DcmFreeError;
use crate::util::wide_nul;

/// True when the current process token is a member of the Administrators group.
#[must_use]
pub fn is_elevated() -> bool {
    // SAFETY: IsUserAnAdmin is callable from any thread on any token.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// RAII wrapper around a `HANDLE` returned by `OpenProcessToken`.
///
/// `CloseHandle` runs on drop, so the handle cannot leak even if a later
/// step panics. Construction is fallible because `OpenProcessToken` can fail.
struct OwnedToken(HANDLE);

impl OwnedToken {
    fn open_current(access: TOKEN_ACCESS_MASK) -> windows::core::Result<Self> {
        let mut h = HANDLE::default();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that does not
        // need closing. `&raw mut h` is a valid, properly-aligned out-pointer
        // for the duration of the call.
        unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut h)? };
        Ok(Self(h))
    }

    const fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedToken {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle was returned by a successful
            // `OpenProcessToken`, and Drop runs at most once per value.
            unsafe { CloseHandle(self.0).ok() };
        }
    }
}

fn priv_err(name: &str, e: &windows::core::Error) -> DcmFreeError {
    DcmFreeError::PrivilegeAcquireFailed {
        privilege: name.to_string(),
        source: io::Error::other(e.to_string()),
    }
}

fn priv_err_msg(name: &str, msg: &str) -> DcmFreeError {
    DcmFreeError::PrivilegeAcquireFailed {
        privilege: name.to_string(),
        source: io::Error::other(msg.to_string()),
    }
}

/// Enable a single named privilege on the current process token.
fn enable_privilege(name: &str) -> Result<(), DcmFreeError> {
    let token = OwnedToken::open_current(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY)
        .map_err(|e| priv_err(name, &e))?;

    let mut luid = LUID::default();
    let name_wide = wide_nul(name);

    // SAFETY: `name_wide` is a NUL-terminated UTF-16 buffer owned by us
    // for the duration of the call. `&raw mut luid` is a valid out-pointer.
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(name_wide.as_ptr()), &raw mut luid) }
        .map_err(|e| priv_err(name, &e))?;

    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };

    // SAFETY: `tp` is initialised and lives for the duration of the call.
    // `token.raw()` is a live handle owned by `token` (Drop closes it).
    unsafe { AdjustTokenPrivileges(token.raw(), false, Some(&raw const tp), 0, None, None) }
        .map_err(|e| priv_err(name, &e))?;

    // AdjustTokenPrivileges reports success even when some privileges were
    // not assigned; the real status is in GetLastError.
    //
    // SAFETY: GetLastError reads the calling thread's last-error code.
    let last = unsafe { GetLastError() };
    if last.is_err() {
        return Err(priv_err_msg(
            name,
            &format!("AdjustTokenPrivileges partial success: {last:?}"),
        ));
    }
    Ok(())
}

/// Enable the two privileges required to destroy HCS layers.
pub fn enable_backup_restore() -> Result<(), DcmFreeError> {
    enable_privilege("SeBackupPrivilege")?;
    enable_privilege("SeRestorePrivilege")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_elevated_does_not_panic() {
        // Either true or false depending on how the test is run; we just want
        // to confirm it doesn't crash with a token error.
        let _ = is_elevated();
    }

    #[test]
    fn enable_privilege_unknown_name_errors() {
        // A made-up privilege name returns a structured error rather than
        // panicking or silently succeeding.
        let err = enable_privilege("SeDefinitelyNotARealPrivilege").unwrap_err();
        assert!(matches!(err, DcmFreeError::PrivilegeAcquireFailed { .. }));
    }

    #[test]
    fn owned_token_drops_invalid_handle_safely() {
        // The Drop impl must be a no-op for an uninitialised (invalid)
        // handle. Constructing one with the default HANDLE exercises that
        // path; CloseHandle must not be called.
        let token = OwnedToken(HANDLE::default());
        assert!(token.raw().is_invalid());
        drop(token);
    }
}
