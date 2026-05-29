use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct WorkspaceIsolation {
    root: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunWorkspace {
    pub run_id: Uuid,
    pub run_dir: PathBuf,
    pub work_dir: PathBuf,
}

impl WorkspaceIsolation {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn create(&self, run_id: Uuid) -> Result<RunWorkspace> {
        let run_dir = self
            .root
            .join(".flow")
            .join("runs")
            .join(run_id.to_string());
        let work_dir = run_dir.join("workspace");
        fs::create_dir_all(&work_dir)
            .with_context(|| format!("failed to create workspace {}", work_dir.display()))?;

        Ok(RunWorkspace {
            run_id,
            run_dir,
            work_dir,
        })
    }
}

impl RunWorkspace {
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.work_dir.join(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_isolated_workspace_per_run() {
        let root = std::env::temp_dir().join(format!("runflow-workspace-{}", Uuid::new_v4()));
        let isolation = WorkspaceIsolation::new(&root);
        let first = isolation.create(Uuid::new_v4()).unwrap();
        let second = isolation.create(Uuid::new_v4()).unwrap();

        assert_ne!(first.work_dir, second.work_dir);
        assert!(first.work_dir.exists());
        assert_eq!(
            first.resolve("artifact.txt"),
            first.work_dir.join("artifact.txt")
        );

        fs::remove_dir_all(root).ok();
    }
}
