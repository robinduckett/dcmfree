//! Optional Docker integration — used to mark layers as in-use so we never
//! destroy a layer Docker still references.
//!
//! If Docker is not installed or not running, this returns an empty set and
//! we proceed as if everything is potentially orphan (the user is told).

use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct InspectGraphDriver {
    #[serde(rename = "Data", default)]
    data: GraphDriverData,
}

#[derive(Debug, Default, Deserialize)]
struct GraphDriverData {
    #[serde(rename = "dir", default)]
    dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InspectRecord {
    #[serde(rename = "GraphDriver", default)]
    graph_driver: Option<InspectGraphDriver>,
}

/// Return the set of layer directories Docker considers in use.
///
/// Includes layers used by all images and containers in the *current* daemon
/// mode. Best-effort: if `docker` isn't on `PATH` or returns an error, the
/// returned set is empty.
pub fn in_use_layer_dirs() -> HashSet<PathBuf> {
    let mut out = HashSet::new();

    if !docker_available() {
        tracing::info!("docker CLI not available; skipping in-use cross-check");
        return out;
    }

    let mut ids = list_ids(&["images"]);
    ids.extend(list_ids(&["ps", "-a"]));

    for id in ids {
        match inspect(&id) {
            Ok(Some(dir)) => {
                out.insert(PathBuf::from(dir));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("docker inspect {id} failed: {e}"),
        }
    }

    out
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn list_ids(subcommand: &[&str]) -> Vec<String> {
    let mut cmd = Command::new("docker");
    cmd.args(subcommand);
    cmd.args(["-q", "--no-trunc"]);
    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn inspect(id: &str) -> std::io::Result<Option<String>> {
    let output = Command::new("docker").args(["inspect", id]).output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let records: Vec<InspectRecord> = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("docker inspect {id}: JSON parse failed: {e}");
            return Ok(None);
        }
    };
    Ok(records
        .into_iter()
        .next()
        .and_then(|r| r.graph_driver)
        .and_then(|g| g.data.dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inspect_record_with_dir() {
        let raw = br#"[
            {
                "Id": "abc123",
                "GraphDriver": {
                    "Name": "windowsfilter",
                    "Data": {
                        "dir": "C:\\ProgramData\\Microsoft\\Windows\\Containers\\Layers\\abc"
                    }
                }
            }
        ]"#;
        let parsed: Vec<InspectRecord> = serde_json::from_slice(raw).unwrap();
        assert_eq!(
            parsed[0].graph_driver.as_ref().unwrap().data.dir.as_deref(),
            Some(r"C:\ProgramData\Microsoft\Windows\Containers\Layers\abc")
        );
    }

    #[test]
    fn parse_inspect_record_without_dir() {
        let raw = br#"[{"Id":"abc"}]"#;
        let parsed: Vec<InspectRecord> = serde_json::from_slice(raw).unwrap();
        assert!(parsed[0].graph_driver.is_none());
    }

    // NB: we deliberately do not test the "docker not on PATH" branch from
    // a unit test — that would require mutating the process-wide PATH env
    // var, which is `unsafe` in Rust 2024 and races with any other test
    // (or runtime thread) reading env vars. The branch is exercised by
    // integration tests that run the binary in an environment without docker.
}
