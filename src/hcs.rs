//! Raw FFI wrapper around `HcsDestroyLayer` from `computestorage.dll`.
//!
//! This is the only supported way to remove an HCS-managed container layer
//! once its reparse points and hardlinks are in place; ordinary
//! `DeleteFile`/`Remove-Item` cannot recurse through them even from an
//! elevated process. The function is not exposed by the official `windows`
//! crate (0.60 has bindings for `computecore.dll` and `computenetwork.dll`
//! under `Win32::System::HostCompute*` but not `computestorage.dll`), so we
//! declare the single function we need by hand.
//!
//! Reference: Windows SDK header `computestorage.h`.
//!
//! ```c
//! HRESULT WINAPI HcsDestroyLayer(
//!     _In_ PCWSTR LayerPath
//! );
//! ```

use crate::errors::DcmFreeError;
use std::path::Path;

#[link(name = "computestorage")]
unsafe extern "system" {
    fn HcsDestroyLayer(layerPath: *const u16) -> i32;
}

/// Destroy a single HCS layer at `path`. Returns Ok(()) on success, or a
/// detailed error containing the HRESULT on failure.
///
/// Caller MUST be elevated and have `SeBackupPrivilege` +
/// `SeRestorePrivilege` enabled on the current token; see
/// [`crate::privileges`].
pub fn destroy_layer(path: &Path) -> Result<(), DcmFreeError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer owned by us for the
    // duration of the call. `HcsDestroyLayer` does not retain the pointer.
    let hr = unsafe { HcsDestroyLayer(wide.as_ptr()) };

    if hr >= 0 {
        Ok(())
    } else {
        Err(DcmFreeError::HcsDestroyFailed {
            path: path.to_path_buf(),
            hresult: hr as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn destroy_nonexistent_layer_does_not_panic() {
        // The HCS API may return either S_OK (treating a missing layer as a
        // no-op success) or an error HRESULT depending on Windows version.
        // We just want to confirm the wrapper handles either path safely
        // without panicking and without UB from the FFI call.
        let p = PathBuf::from(r"C:\definitely-not-a-real-layer-12345-dcmfree-test");
        match destroy_layer(&p) {
            Ok(()) => {}
            Err(DcmFreeError::HcsDestroyFailed { .. }) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }
}
