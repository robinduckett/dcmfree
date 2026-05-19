use std::path::PathBuf;

use thiserror::Error;

/// Errors emitted by `dcmfree`.
#[derive(Debug, Error)]
pub enum DcmFreeError {
    #[error("layers directory not found: {0}")]
    LayersDirMissing(PathBuf),

    #[error("not running as Administrator; HCS layer destruction requires elevation")]
    NotElevated,

    #[error("failed to acquire privilege {privilege}: {source}")]
    PrivilegeAcquireFailed {
        privilege: String,
        #[source]
        source: std::io::Error,
    },

    #[error("HcsDestroyLayer failed for {path}: HRESULT 0x{hresult:08X}")]
    HcsDestroyFailed { path: PathBuf, hresult: u32 },

    #[error("HCS operation failed ({operation}): HRESULT 0x{hresult:08X}{}",
        if details.is_empty() { String::new() } else { format!(" — {details}") })]
    HcsOperationFailed {
        operation: String,
        hresult: u32,
        details: String,
    },

    #[error("docker query failed: {0}")]
    DockerQueryFailed(String),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DcmFreeError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
