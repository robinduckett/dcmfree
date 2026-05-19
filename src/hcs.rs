//! Host Compute Service (HCS) API wrapper.
//!
//! Every function we call is now sourced from the official `windows` crate's
//! typed bindings under `Win32::System::HostComputeSystem` (functions) and
//! `Win32::System::Memory` (`LocalFree`). That gives us:
//!
//! - `HRESULT -> windows::core::Result<()>` auto-conversion,
//! - `HCS_OPERATION` and `HLOCAL` as proper newtypes (no `*mut c_void`),
//! - `PCWSTR` / `PWSTR` for strings (NUL termination is a type invariant).
//!
//! Higher-level abstractions:
//!
//! - [`HcsOperation`] — RAII wrapper around `HCS_OPERATION`; `Drop` calls
//!   `HcsCloseOperation` exactly once and idempotently.
//! - [`HcsApi`] — zero-sized handle that exposes the user-facing methods
//!   `destroy_layer` and `enumerate_compute_systems`.

use crate::errors::DcmFreeError;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::System::HostComputeSystem::{
    HCS_OPERATION, HcsCloseOperation, HcsCreateOperation, HcsDestroyLayer,
    HcsEnumerateComputeSystems, HcsWaitForOperationResult,
};
use windows::core::{PCWSTR, PWSTR};

// ---- public types ----------------------------------------------------------

/// One entry returned by `HcsEnumerateComputeSystems`.
///
/// The HCS JSON schema contains many optional fields; we deserialise only the
/// ones useful for diagnostics. Unknown fields are silently ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct ComputeSystemSummary {
    #[serde(rename = "Id", default)]
    pub id: String,
    #[serde(rename = "SystemType", default)]
    pub system_type: Option<String>,
    #[serde(rename = "Name", default)]
    pub name: Option<String>,
    #[serde(rename = "Owner", default)]
    pub owner: Option<String>,
    #[serde(rename = "State", default)]
    pub state: Option<String>,
    #[serde(rename = "RuntimeId", default)]
    pub runtime_id: Option<String>,
    #[serde(rename = "RuntimeImagePath", default)]
    pub runtime_image_path: Option<String>,
    #[serde(rename = "RuntimeTemplateId", default)]
    pub runtime_template_id: Option<String>,
}

/// Top-level wrapper for HCS API calls. Zero-sized; instances are cheap.
#[derive(Debug, Default, Clone, Copy)]
pub struct HcsApi;

impl HcsApi {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Destroy a single HCS layer at `path`. Synchronous — does not use an
    /// operation handle. Caller MUST be elevated and have `SeBackupPrivilege`
    /// + `SeRestorePrivilege` enabled on the current token.
    pub fn destroy_layer(self, path: &Path) -> Result<(), DcmFreeError> {
        let wide = to_wide(&path.as_os_str().to_string_lossy());

        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer owned by us for
        // the duration of the call; `HcsDestroyLayer` does not retain it.
        let result = unsafe { HcsDestroyLayer(PCWSTR(wide.as_ptr())) };
        result.map_err(|e| {
            #[allow(clippy::cast_sign_loss)]
            let hresult = e.code().0 as u32;
            DcmFreeError::HcsDestroyFailed {
                path: path.to_path_buf(),
                hresult,
            }
        })
    }

    /// Enumerate every active HCS compute system on the machine.
    ///
    /// The empty query asks for everything; equivalent to `hcsdiag list`. The
    /// returned vector is empty when nothing is running. Each entry is a
    /// compute system known to the HCS service — containers (Windows-mode
    /// Docker, containerd), Windows Sandbox sessions, and Hyper-V isolated
    /// utility VMs.
    pub fn enumerate_compute_systems(self) -> Result<Vec<ComputeSystemSummary>, DcmFreeError> {
        let op = HcsOperation::new()?;
        let empty_query: [u16; 1] = [0];

        // SAFETY: `empty_query` is a valid NUL-terminated UTF-16 buffer
        // owned for the duration of the call. `op.handle` is a live
        // operation handle (Drop closes it).
        unsafe { HcsEnumerateComputeSystems(PCWSTR(empty_query.as_ptr()), op.handle) }
            .map_err(|e| make_op_error("HcsEnumerateComputeSystems", &e, ""))?;

        let json = op.wait_for_result(Some(Duration::from_secs(15)))?;
        if json.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&json).map_err(|e| DcmFreeError::HcsOperationFailed {
            operation: "HcsEnumerateComputeSystems".into(),
            hresult: 0,
            details: format!("invalid JSON response: {e}"),
        })
    }
}

/// RAII wrapper around an `HCS_OPERATION` handle.
///
/// `HcsCloseOperation` runs on drop, so an `HcsOperation` value is the only
/// thing that needs to live for the duration of an async call.
pub struct HcsOperation {
    handle: HCS_OPERATION,
}

impl HcsOperation {
    /// Allocate a new operation handle.
    ///
    /// Returns an error only on resource exhaustion — the API returns a null
    /// handle in that case.
    pub fn new() -> Result<Self, DcmFreeError> {
        // SAFETY: passing None for both context and callback requests a
        // "polling" operation we'll consume via `HcsWaitForOperationResult`.
        let handle = unsafe { HcsCreateOperation(None, None) };
        if handle.is_invalid() {
            return Err(DcmFreeError::HcsOperationFailed {
                operation: "HcsCreateOperation".into(),
                hresult: 0,
                details: "API returned null operation handle".into(),
            });
        }
        Ok(Self { handle })
    }

    /// Block until the operation completes (or the timeout elapses) and
    /// return the result-document JSON string.
    ///
    /// `timeout = None` uses `INFINITE`. The result document is allocated by
    /// the HCS API with `LocalAlloc`; ownership is transferred to an
    /// [`OwnedLocalAlloc`] immediately on return so the buffer is freed even
    /// if a subsequent step (UTF-16 decode, allocation) panics.
    pub fn wait_for_result(&self, timeout: Option<Duration>) -> Result<String, DcmFreeError> {
        let timeout_ms = timeout.map_or(u32::MAX, |d| {
            u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
        });

        let mut raw_doc = PWSTR::null();

        // SAFETY: `&raw mut raw_doc` is a valid, properly-aligned out-pointer
        // for the duration of the call; on success the API populates it with
        // a `LocalAlloc`-owned buffer.
        let api_result =
            unsafe { HcsWaitForOperationResult(self.handle, timeout_ms, Some(&raw mut raw_doc)) };

        // Take ownership of the buffer (if any) IMMEDIATELY so any subsequent
        // panic during string decoding still frees it via Drop.
        let owned = OwnedLocalAlloc::from_pwstr(raw_doc);
        let text = owned.to_string_lossy();

        match api_result {
            Ok(()) => Ok(text),
            Err(e) => Err(make_op_error("HcsWaitForOperationResult", &e, &text)),
        }
    }
}

impl Drop for HcsOperation {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: handle was allocated by `HcsCreateOperation` and we
            // only call close once (Drop is one-shot per `HcsOperation`).
            unsafe { HcsCloseOperation(self.handle) };
        }
    }
}

// ---- helpers ---------------------------------------------------------------

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// RAII wrapper around a `LocalAlloc`-backed UTF-16 buffer returned from an
/// HCS API. Drop calls `LocalFree`, so the buffer cannot leak even if a
/// later step (string decode, allocation) panics.
struct OwnedLocalAlloc(*mut u16);

impl OwnedLocalAlloc {
    /// Take ownership of the buffer behind a `PWSTR`. `PWSTR::null()` is
    /// represented internally as a null pointer, which makes `Drop` a no-op.
    const fn from_pwstr(p: PWSTR) -> Self {
        Self(p.0)
    }

    const fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Decode the NUL-terminated UTF-16 buffer to a `String`, lossily on
    /// invalid sequences. Returns the empty string for null buffers.
    fn to_string_lossy(&self) -> String {
        if self.is_null() {
            return String::new();
        }
        // SAFETY: the buffer was produced by an HCS API call; it points to
        // a NUL-terminated UTF-16 sequence. `PWSTR::to_string` walks to the
        // first NUL and returns `Err` only on invalid UTF-16.
        unsafe { PWSTR(self.0).to_string() }.unwrap_or_default()
    }
}

impl Drop for OwnedLocalAlloc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer originated from an HCS API call that used
            // `LocalAlloc`, and Drop runs at most once per value.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.cast())));
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

fn make_op_error(op: &str, err: &windows::core::Error, details: &str) -> DcmFreeError {
    #[allow(clippy::cast_sign_loss)]
    let hresult = err.code().0 as u32;
    DcmFreeError::HcsOperationFailed {
        operation: op.into(),
        hresult,
        details: details.into(),
    }
}

// ---- compatibility shim ----------------------------------------------------
// The original free function is retained so existing call sites keep working.

/// Destroy a single HCS layer at `path` via the default [`HcsApi`] instance.
pub fn destroy_layer(path: &Path) -> Result<(), DcmFreeError> {
    HcsApi::new().destroy_layer(path)
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn destroy_nonexistent_layer_does_not_panic() {
        let p = PathBuf::from(r"C:\definitely-not-a-real-layer-12345-dcmfree-test");
        match destroy_layer(&p) {
            // Either outcome is acceptable; the HCS API treats a missing
            // layer as either a no-op success or returns an error HRESULT
            // depending on the Windows build. We just confirm we get one
            // of the two structured outcomes, not a panic.
            Ok(()) | Err(DcmFreeError::HcsDestroyFailed { .. }) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn operation_can_be_created_and_dropped() {
        let op = HcsOperation::new().expect("create operation");
        drop(op);
    }

    #[test]
    fn enumerate_returns_a_well_formed_vec() {
        // We don't know what's running on the test machine, but the call
        // must succeed and return a Vec (possibly empty). Any structured
        // error variant is also acceptable (e.g. on a stripped Server SKU
        // without HCS).
        match HcsApi::new().enumerate_compute_systems() {
            Ok(v) => {
                for sys in &v {
                    assert!(
                        !sys.id.is_empty() || sys.system_type.is_some(),
                        "expected at least one identifying field, got {sys:?}"
                    );
                }
            }
            Err(DcmFreeError::HcsOperationFailed { .. }) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn deserialise_typical_enumerate_payload() {
        let raw = r#"[
            {
                "Id": "abc123",
                "SystemType": "Container",
                "Name": "my-container",
                "Owner": "docker",
                "State": "Running",
                "RuntimeId": "00000000-0000-0000-0000-000000000000",
                "RuntimeImagePath": "C:\\ProgramData\\Microsoft\\Windows\\Containers\\Layers\\abc",
                "Extra": "ignored"
            },
            {
                "Id": "def456",
                "SystemType": "VirtualMachine",
                "State": "Stopped"
            }
        ]"#;
        let parsed: Vec<ComputeSystemSummary> = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "abc123");
        assert_eq!(parsed[0].state.as_deref(), Some("Running"));
        assert!(
            parsed[0]
                .runtime_image_path
                .as_deref()
                .unwrap_or("")
                .contains("Containers")
        );
        assert_eq!(parsed[1].system_type.as_deref(), Some("VirtualMachine"));
        assert!(parsed[1].name.is_none());
    }

    #[test]
    fn deserialise_empty_enumerate_payload() {
        let parsed: Vec<ComputeSystemSummary> = serde_json::from_str("[]").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn owned_local_alloc_drops_null_safely() {
        // Drop on a null buffer must be a no-op; never call LocalFree(NULL).
        let owned = OwnedLocalAlloc::from_pwstr(PWSTR::null());
        assert!(owned.is_null());
        assert_eq!(owned.to_string_lossy(), "");
        drop(owned);
    }
}
