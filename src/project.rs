use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub id: String,
    pub root: PathBuf,
}

impl Project {
    pub fn discover(cwd: &Path) -> Result<Self, ProjectError> {
        let cwd = cwd
            .canonicalize()
            .map_err(|source| ProjectError::Canonicalize {
                path: cwd.to_path_buf(),
                source,
            })?;
        let root = git_root(&cwd).unwrap_or(cwd);
        let hash = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();

        Ok(Self {
            id: hash[..16].to_owned(),
            root,
        })
    }
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let root = String::from_utf8(output.stdout).ok()?;
    PathBuf::from(root.trim()).canonicalize().ok()
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("failed to resolve project path {path}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::Project;

    #[test]
    fn discover_uses_canonical_directory_outside_git_repository() {
        let directory = tempdir().expect("temporary directory should be created");

        let project = Project::discover(directory.path()).expect("project should be discovered");

        assert_eq!(
            project.root,
            directory
                .path()
                .canonicalize()
                .expect("temporary directory should resolve")
        );
    }

    #[test]
    fn discover_returns_stable_id_for_same_directory() {
        let directory = tempdir().expect("temporary directory should be created");
        let first =
            Project::discover(directory.path()).expect("first project should be discovered");

        let second =
            Project::discover(directory.path()).expect("second project should be discovered");

        assert_eq!(first.id, second.id);
    }
}
