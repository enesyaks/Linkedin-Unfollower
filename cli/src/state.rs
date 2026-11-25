//! The resume checkpoint.
//!
//! Written after every single request, so a Ctrl-C, a crash or a rate-limit
//! wall never costs you more than the one request in flight.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    pub key: String,
    pub label: String,
    pub status: Option<u16>,
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub done: BTreeSet<String>,
    #[serde(default)]
    pub failed: Vec<Failure>,
    #[serde(default)]
    pub updated_at_unix: u64,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl State {
    /// Loads the checkpoint, or starts a fresh one if the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let mut state = match std::fs::read_to_string(path) {
            Ok(raw) if raw.trim().is_empty() => State::default(),
            Ok(raw) => serde_json::from_str(&raw)
                .with_context(|| format!("corrupt state file: {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        state.path = Some(path.to_path_buf());
        Ok(state)
    }

    pub fn is_done(&self, key: &str) -> bool {
        self.done.contains(key)
    }

    pub fn mark_done(&mut self, key: &str) {
        self.done.insert(key.to_string());
        self.failed.retain(|f| f.key != key);
    }

    pub fn mark_failed(&mut self, failure: Failure) {
        self.failed.retain(|f| f.key != failure.key);
        self.failed.push(failure);
    }

    /// Atomic save: write a sibling temp file, then rename over the target.
    pub fn save(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        self.updated_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, json).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "liunfollow-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn missing_file_starts_empty() {
        let path = tmpdir().join("absent.json");
        let _ = std::fs::remove_file(&path);
        let s = State::load(&path).unwrap();
        assert!(s.done.is_empty());
        assert!(s.failed.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let path = tmpdir().join("round-trip.json");
        let _ = std::fs::remove_file(&path);

        let mut s = State::load(&path).unwrap();
        s.mark_done("urn:li:fsd_company:1");
        s.mark_failed(Failure {
            key: "urn:li:fsd_company:2".into(),
            label: "Globex".into(),
            status: Some(422),
            reason: "unprocessable".into(),
        });
        s.save().unwrap();

        let again = State::load(&path).unwrap();
        assert!(again.is_done("urn:li:fsd_company:1"));
        assert_eq!(again.failed.len(), 1);
        assert_eq!(again.failed[0].status, Some(422));
        assert!(again.updated_at_unix > 0);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "temp file left behind"
        );
    }

    #[test]
    fn a_later_success_clears_an_earlier_failure() {
        let path = tmpdir().join("retry.json");
        let _ = std::fs::remove_file(&path);
        let mut s = State::load(&path).unwrap();
        s.mark_failed(Failure {
            key: "k".into(),
            label: "K".into(),
            status: Some(500),
            reason: "server error".into(),
        });
        s.mark_done("k");
        assert!(s.failed.is_empty());
        assert!(s.is_done("k"));
    }

    #[test]
    fn failures_are_recorded_once_per_company() {
        let mut s = State::default();
        for i in 0..3 {
            s.mark_failed(Failure {
                key: "k".into(),
                label: "K".into(),
                status: Some(500),
                reason: format!("attempt {i}"),
            });
        }
        assert_eq!(s.failed.len(), 1);
        assert_eq!(s.failed[0].reason, "attempt 2");
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_silent_reset() {
        let path = tmpdir().join("corrupt.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(State::load(&path).is_err());
    }

    #[test]
    fn empty_file_is_treated_as_fresh() {
        let path = tmpdir().join("empty.json");
        std::fs::write(&path, "   \n").unwrap();
        assert!(State::load(&path).unwrap().done.is_empty());
    }
}
