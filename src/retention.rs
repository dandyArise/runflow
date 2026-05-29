use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use uuid::Uuid;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionPolicy {
    pub keep_runs: Option<usize>,
    pub older_than_days: Option<u64>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionReport {
    pub scanned_runs: usize,
    pub removed_runs: usize,
    pub removed_files: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RunCandidate {
    run_id: Uuid,
    path: PathBuf,
    modified_at: SystemTime,
}

pub fn run_retention(root: impl AsRef<Path>, policy: RetentionPolicy) -> Result<RetentionReport> {
    let root = root.as_ref();
    let mut candidates = collect_runs(root)?;
    candidates.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });

    let scanned_runs = candidates.len();
    let purge = select_purge_candidates(&candidates, &policy);
    let mut removed_files = 0;

    if !policy.dry_run {
        for run in &purge {
            removed_files += remove_run_artifacts(root, run)?;
        }
    }

    Ok(RetentionReport {
        scanned_runs,
        removed_runs: purge.len(),
        removed_files,
        dry_run: policy.dry_run,
    })
}

fn collect_runs(root: &Path) -> Result<Vec<RunCandidate>> {
    let runs_dir = root.join(".flow").join("runs");
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }

    let mut runs = Vec::new();
    for entry in fs::read_dir(&runs_dir)
        .with_context(|| format!("failed to read runs directory {}", runs_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(run_id) = Uuid::parse_str(&name) else {
            continue;
        };
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read run metadata {}", entry.path().display()))?;
        let modified_at = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        runs.push(RunCandidate {
            run_id,
            path: entry.path(),
            modified_at,
        });
    }
    Ok(runs)
}

fn select_purge_candidates(
    candidates: &[RunCandidate],
    policy: &RetentionPolicy,
) -> Vec<RunCandidate> {
    let cutoff = policy
        .older_than_days
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .and_then(|seconds| SystemTime::now().checked_sub(Duration::from_secs(seconds)));

    candidates
        .iter()
        .enumerate()
        .filter(|(index, run)| {
            let exceeds_keep = policy
                .keep_runs
                .is_some_and(|keep_runs| *index >= keep_runs);
            let older_than_cutoff = cutoff.is_some_and(|cutoff| run.modified_at < cutoff);

            match (policy.keep_runs, policy.older_than_days) {
                (Some(_), Some(_)) => exceeds_keep && older_than_cutoff,
                (Some(_), None) => exceeds_keep,
                (None, Some(_)) => older_than_cutoff,
                (None, None) => false,
            }
        })
        .map(|(_, run)| run.clone())
        .collect()
}

fn remove_run_artifacts(root: &Path, run: &RunCandidate) -> Result<usize> {
    let mut removed = 0;
    if run.path.exists() {
        fs::remove_dir_all(&run.path)
            .with_context(|| format!("failed to remove run directory {}", run.path.display()))?;
        removed += 1;
    }

    let snapshots_dir = root.join(".flow").join("snapshots");
    for path in [
        snapshots_dir.join(format!("{}.snapshot", run.run_id)),
        snapshots_dir.join(format!("{}.snapshot.meta", run.run_id)),
    ] {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove snapshot {}", path.display()))?;
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir(root: &Path, run_id: Uuid) -> PathBuf {
        root.join(".flow").join("runs").join(run_id.to_string())
    }

    #[test]
    fn dry_run_reports_without_removing() {
        let root = std::env::temp_dir().join(format!("runflow-retention-{}", Uuid::new_v4()));
        let run_id = Uuid::new_v4();
        fs::create_dir_all(run_dir(&root, run_id)).unwrap();

        let report = run_retention(
            &root,
            RetentionPolicy {
                keep_runs: Some(0),
                older_than_days: None,
                dry_run: true,
            },
        )
        .unwrap();

        assert_eq!(report.scanned_runs, 1);
        assert_eq!(report.removed_runs, 1);
        assert_eq!(report.removed_files, 0);
        assert!(run_dir(&root, run_id).exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn removes_runs_and_snapshots_beyond_keep_limit() {
        let root = std::env::temp_dir().join(format!("runflow-retention-{}", Uuid::new_v4()));
        let run_id = Uuid::new_v4();
        let snapshots = root.join(".flow").join("snapshots");
        fs::create_dir_all(run_dir(&root, run_id)).unwrap();
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join(format!("{run_id}.snapshot")), "{}").unwrap();
        fs::write(snapshots.join(format!("{run_id}.snapshot.meta")), "{}").unwrap();

        let report = run_retention(
            &root,
            RetentionPolicy {
                keep_runs: Some(0),
                older_than_days: None,
                dry_run: false,
            },
        )
        .unwrap();

        assert_eq!(report.scanned_runs, 1);
        assert_eq!(report.removed_runs, 1);
        assert_eq!(report.removed_files, 3);
        assert!(!run_dir(&root, run_id).exists());
        assert!(!snapshots.join(format!("{run_id}.snapshot")).exists());
        assert!(!snapshots.join(format!("{run_id}.snapshot.meta")).exists());

        fs::remove_dir_all(root).ok();
    }
}
