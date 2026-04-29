use std::path::{Path, PathBuf};
use std::process::Command;

use crate::session::ProviderSession;

pub fn session_working_dir(session: Option<&ProviderSession>) -> Option<&str> {
    session
        .and_then(|session| session.working_dir.as_deref())
        .filter(|dir| !dir.trim().is_empty())
}

pub fn apply_session_working_dir(cmd: &mut Command, session: Option<&ProviderSession>) {
    if let Some(dir) = session_working_dir(session) {
        cmd.current_dir(dir);
    }
}

pub fn project_root_for_session(session: Option<&ProviderSession>) -> PathBuf {
    session_working_dir(session)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn should_skip_codex_git_repo_check(prefixes: &[PathBuf], working_dir: Option<&str>) -> bool {
    let Some(dir) = working_dir else {
        return false;
    };
    let Ok(canonical_dir) = Path::new(dir).canonicalize() else {
        return false;
    };

    prefixes
        .iter()
        .filter_map(|prefix| prefix.canonicalize().ok())
        .any(|prefix| canonical_dir.starts_with(&prefix))
        && !is_inside_git_repo(&canonical_dir)
}

fn is_inside_git_repo(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
}
