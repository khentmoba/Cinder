//! Project identity for a run: which repository was the agent working in?
//!
//! Agents record the working directory they ran in. Modern workflows put that
//! directory inside a git worktree, so the same repository would otherwise be
//! counted as several unrelated projects. This module walks up to the
//! repository root and resolves a worktree back to its main checkout, so every
//! branch of a repository rolls up into one project.

use std::fs;
use std::path::{Path, PathBuf};

/// The repository a working directory belongs to.
///
/// * `.git` as a **directory** is the main checkout: the repository root is the
///   directory that holds it.
/// * `.git` as a **file** points at `gitdir: <repo>/.git/worktrees/<name>`.
///   The repository root is the grandparent of `worktrees/<name>`, i.e. three
///   levels up from the named worktree directory.
///
/// Returns `None` when no `.git` is found up to the filesystem root.
pub fn canonical_repo(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return Some(d.to_path_buf());
        }
        if dot_git.is_file() {
            if let Some(root) = repo_from_gitdir(&dot_git) {
                return Some(root);
            }
        }
        dir = d.parent();
    }
    None
}

/// Resolve a `.git` *file* to the repository root it points at.
fn repo_from_gitdir(dot_git: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let rest = text.trim().strip_prefix("gitdir:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let target = PathBuf::from(rest);
    // A relative gitdir is resolved against the directory holding the file.
    let target = if target.is_absolute() {
        target
    } else {
        dot_git.parent()?.join(target)
    };
    let target = normalize(&target);
    // <repo>/.git/worktrees/<name> -> <repo>
    if target.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new("worktrees")) {
        return target
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
    }
    // <repo>/.git -> <repo>
    if target.file_name() == Some(std::ffi::OsStr::new(".git")) {
        return target.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Resolve `.` and `..` in a path without touching the filesystem.
///
/// A relative gitdir such as `../.git` would otherwise leave a `..` in the
/// middle of the path, and the label would come out as `plain\..`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The label shown for a project: the repository's directory name.
///
/// A directory with no repository falls back to its own name, and an empty
/// working directory yields an empty label (the row is then not attributed to
/// any project).
pub fn project_label(cwd: &Path) -> String {
    if cwd.as_os_str().is_empty() {
        return String::new();
    }
    if let Some(root) = canonical_repo(cwd) {
        return root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    // A checkout that has been cleaned up still belongs to the project that
    // owns the worktrees folder it was in, and its .git file is long gone.
    if let Some(owner) = worktree_owner(cwd) {
        return owner;
    }
    cwd.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// The project that owns a checkout under a `worktrees` folder.
///
/// The layout is `<root>/worktrees/<project>/<branch>`, so the project is the
/// directory above the `worktrees` folder.
fn worktree_owner(dir: &Path) -> Option<String> {
    let parent = dir.parent()?;
    if parent.parent().and_then(|p| p.file_name()) != Some(std::ffi::OsStr::new("worktrees")) {
        return None;
    }
    parent.file_name().map(|n| n.to_string_lossy().to_string())
}

/// The label for a run, given the working directory the agent reported.
pub fn label_for_cwd(cwd: Option<&str>) -> String {
    match cwd {
        Some(c) if !c.trim().is_empty() => project_label(Path::new(c)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_checkout_is_its_own_root() {
        let tmp = std::env::temp_dir().join("cinder-project-root");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join(".git")).unwrap();
        fs::create_dir_all(tmp.join("src")).unwrap();
        assert_eq!(canonical_repo(&tmp.join("src")), Some(tmp.clone()));
        assert_eq!(project_label(&tmp.join("src")), tmp.file_name().unwrap().to_string_lossy());
    }

    #[test]
    fn a_worktree_rolls_up_to_its_main_checkout() {
        let tmp = std::env::temp_dir().join("cinder-project-worktree");
        let _ = fs::remove_dir_all(&tmp);
        let repo = tmp.join("mainrepo");
        let wt = repo.join(".git").join("worktrees").join("feature");
        fs::create_dir_all(&wt).unwrap();
        let checkout = tmp.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        // The worktree's .git file points back into the main checkout.
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", wt.to_string_lossy()),
        )
        .unwrap();
        assert_eq!(canonical_repo(&checkout), Some(repo.clone()));
        assert_eq!(project_label(&checkout), "mainrepo");
    }

    #[test]
    fn a_plain_dot_git_file_resolves_to_its_parent() {
        let tmp = std::env::temp_dir().join("cinder-project-gitfile");
        let _ = fs::remove_dir_all(&tmp);
        let repo = tmp.join("plain");
        let sub = repo.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        // A checkout whose .git file points at the repository's git directory.
        fs::write(sub.join(".git"), "gitdir: ../../.git\n").unwrap();
        assert_eq!(canonical_repo(&sub), Some(repo));
    }

    #[test]
    fn a_directory_without_a_repository_keeps_its_own_name() {
        let tmp = std::env::temp_dir().join("cinder-project-norepo");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("scratch")).unwrap();
        assert_eq!(canonical_repo(&tmp.join("scratch")), None);
        assert_eq!(project_label(&tmp.join("scratch")), "scratch");
    }

    #[test]
    fn an_empty_working_directory_has_no_project() {
        assert_eq!(label_for_cwd(None), "");
        assert_eq!(label_for_cwd(Some("   ")), "");
    }

    #[test]
    fn a_removed_worktree_still_belongs_to_its_project() {
        let tmp = std::env::temp_dir().join("cinder-project-gone");
        let _ = fs::remove_dir_all(&tmp);
        let branch = tmp.join("worktrees").join("Cinder").join("t3-1e48cab7");
        // The checkout is gone, so there is no .git file to read.
        assert_eq!(canonical_repo(&branch), None);
        assert_eq!(project_label(&branch), "Cinder");
    }
}
