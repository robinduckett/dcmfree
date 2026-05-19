//! Acquire `SeBackupPrivilege` and `SeRestorePrivilege` for the current process
//! so HCS APIs can traverse and delete the container layer reparse points and
//! hardlinks. These privileges are *present* on Administrator tokens but not
//! *enabled* by default — `AdjustTokenPrivileges` flips them on.

use crate::errors::DcmFreeError;
use std::io;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::IsUserAnAdmin;

/// True when the current process token is a member of the Administrators group.
pub fn is_elevated() -> bool {
    // SAFETY: IsUserAnAdmin is callable from any thread on any token.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Enable a single named privilege on the current process token.
fn enable_privilege(name: &str) -> Result<(), DcmFreeError> {
    let mut token = HANDLE::default();

    // SAFETY: process handle from GetCurrentProcess is a pseudohandle and
    // does not need closing. `token` is closed below before any early return.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .map_err(|e| DcmFreeError::PrivilegeAcquireFailed {
        privilege: name.to_string(),
        source: io::Error::other(e.to_string()),
    })?;

    let result = (|| -> Result<(), DcmFreeError> {
        let mut luid = LUID::default();
        let name_wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();

        unsafe { LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(name_wide.as_ptr()), &mut luid) }
            .map_err(|e| DcmFreeError::PrivilegeAcquireFailed {
                privilege: name.to_string(),
                source: io::Error::other(e.to_string()),
            })?;

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        let adjusted = unsafe { AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None) };
        adjusted.map_err(|e| DcmFreeError::PrivilegeAcquireFailed {
            privilege: name.to_string(),
            source: io::Error::other(e.to_string()),
        })?;

        // AdjustTokenPrivileges returns success even if one or more privileges
        // were not assigned; the actual outcome is in GetLastError.
        let last = unsafe { GetLastError() };
        if last.is_err() {
            return Err(DcmFreeError::PrivilegeAcquireFailed {
                privilege: name.to_string(),
                source: io::Error::other(format!(
                    "AdjustTokenPrivileges partial success: {:?}",
                    last
                )),
            });
        }
        Ok(())
    })();

    unsafe { CloseHandle(token).ok() };

    result
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
}
