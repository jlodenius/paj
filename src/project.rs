use std::collections::BTreeSet;
use std::env;
use std::fs;
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

    pub fn resolve(reference: &str) -> Result<Self, ProjectError> {
        Self::resolve_in(reference, project_search_roots()?)
    }

    pub fn resolve_in(reference: &str, roots: Vec<PathBuf>) -> Result<Self, ProjectError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(ProjectError::EmptyReference);
        }
        let roots = canonical_roots(roots);
        let reference_path = Path::new(reference);
        let mut direct = BTreeSet::new();
        if reference_path.is_absolute() && reference_path.is_dir() {
            direct.insert(reference_path.canonicalize().map_err(|source| {
                ProjectError::Canonicalize {
                    path: reference_path.to_path_buf(),
                    source,
                }
            })?);
        }
        for root in &roots {
            if root.file_name().is_some_and(|name| name == reference) {
                direct.insert(root.clone());
            }
            let candidate = root.join(reference_path);
            if candidate.is_dir() {
                direct.insert(candidate.canonicalize().map_err(|source| {
                    ProjectError::Canonicalize {
                        path: candidate,
                        source,
                    }
                })?);
            }
        }
        let candidates = if direct.is_empty() {
            let mut found = BTreeSet::new();
            for root in &roots {
                find_matches(root, root, reference_path, &mut found)?;
            }
            found
        } else {
            direct
        };
        let projects = candidates
            .into_iter()
            .map(|path| Self::discover(&path))
            .collect::<Result<Vec<_>, _>>()?;
        let projects: Vec<Project> =
            projects
                .into_iter()
                .fold(Vec::new(), |mut unique, project| {
                    if !unique.iter().any(|item| item.root == project.root) {
                        unique.push(project);
                    }
                    unique
                });
        match projects.as_slice() {
            [] => Err(ProjectError::NotFound(reference.to_owned())),
            [project] => Ok(project.clone()),
            _ => Err(ProjectError::Ambiguous {
                reference: reference.to_owned(),
                candidates: projects.into_iter().map(|project| project.root).collect(),
            }),
        }
    }
}

fn project_search_roots() -> Result<Vec<PathBuf>, ProjectError> {
    let configured = env::var("PAJ_PROJECT_DIRS").unwrap_or_default();
    let values = if configured.trim().is_empty() {
        vec!["~/Development".to_owned()]
    } else {
        configured.split(',').map(str::to_owned).collect()
    };
    let home = env::var_os("HOME").map(PathBuf::from);
    Ok(values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            if value == "~" {
                home.clone()
            } else if let Some(rest) = value.strip_prefix("~/") {
                home.as_ref().map(|path| path.join(rest))
            } else {
                Some(PathBuf::from(value))
            }
        })
        .collect())
}

fn canonical_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .filter(|root| root.is_dir())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn find_matches(
    root: &Path,
    directory: &Path,
    reference: &Path,
    found: &mut BTreeSet<PathBuf>,
) -> Result<(), ProjectError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if [".git", "node_modules", ".direnv", "target"]
            .iter()
            .any(|pruned| name == *pruned)
        {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked path should be under root");
        if name == reference.as_os_str() || relative.ends_with(reference) {
            found.insert(
                path.canonicalize()
                    .map_err(|source| ProjectError::Canonicalize {
                        path: path.clone(),
                        source,
                    })?,
            );
        }
        find_matches(root, &path, reference, found)?;
    }
    Ok(())
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
    #[error("project reference cannot be empty")]
    EmptyReference,
    #[error("no project exactly matches {0}")]
    NotFound(String),
    #[error("project reference {reference} is ambiguous; candidates: {candidates:?}")]
    Ambiguous {
        reference: String,
        candidates: Vec<PathBuf>,
    },
    #[error("failed to resolve project path {path}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
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

    #[test]
    fn resolve_finds_an_exact_nested_directory_name() {
        let directory = tempdir().expect("temporary directory should be created");
        let project = directory.path().join("teams/example");
        std::fs::create_dir_all(&project).expect("project should be created");

        let resolved = Project::resolve_in("example", vec![directory.path().to_path_buf()])
            .expect("exact project should resolve");

        assert_eq!(
            resolved.root,
            project.canonicalize().expect("project should resolve")
        );
    }

    #[test]
    fn resolve_reports_ambiguous_exact_names() {
        let directory = tempdir().expect("temporary directory should be created");
        std::fs::create_dir_all(directory.path().join("one/example")).expect("first should exist");
        std::fs::create_dir_all(directory.path().join("two/example")).expect("second should exist");

        let result = Project::resolve_in("example", vec![directory.path().to_path_buf()]);

        assert!(matches!(result, Err(super::ProjectError::Ambiguous { .. })));
    }

    #[test]
    fn resolve_deduplicates_overlapping_roots_and_prunes_dependencies() {
        let directory = tempdir().expect("temporary directory should be created");
        let project = directory.path().join("group/example");
        std::fs::create_dir_all(&project).expect("project should exist");
        std::fs::create_dir_all(directory.path().join("node_modules/example"))
            .expect("dependency should exist");

        let resolved = Project::resolve_in(
            "example",
            vec![
                directory.path().to_path_buf(),
                directory.path().join("group"),
            ],
        )
        .expect("overlapping roots should deduplicate");

        assert_eq!(
            resolved.root,
            project.canonicalize().expect("project should resolve")
        );
    }

    #[test]
    fn direct_relative_suffix_takes_precedence_over_recursive_names() {
        let directory = tempdir().expect("temporary directory should be created");
        let direct = directory.path().join("team/example");
        std::fs::create_dir_all(&direct).expect("direct project should exist");
        std::fs::create_dir_all(directory.path().join("other/example"))
            .expect("other project should exist");

        let resolved = Project::resolve_in("team/example", vec![directory.path().to_path_buf()])
            .expect("direct suffix should resolve");

        assert_eq!(
            resolved.root,
            direct.canonicalize().expect("project should resolve")
        );
    }
}
