use std::fs;
use std::path::{Path, PathBuf};
fn looks_like_thread_uuid(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(idx, ch)| match idx {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
}
fn rollout_search_roots(home: &Path) -> [PathBuf; 2] {
    [home.join("sessions"), home.join("archived_sessions")]
}
fn home_contains_rollout(home: &Path, session_id: &str) -> bool {
    let suffix = format!("-{session_id}.jsonl");
    rollout_search_roots(home)
        .iter()
        .any(|root| tree_contains_rollout(root, &suffix))
}
fn tree_contains_rollout(root: &Path, suffix: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if tree_contains_rollout(&path, suffix) {
                return true;
            }
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.ends_with(suffix) {
            return true;
        }
    }

    false
}
fn legacy_codex_home() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".codex"))
}
pub(crate) fn select_codex_home_for_resume(
    configured_home: Option<&Path>,
    resume_session_id: Option<&str>,
) -> Option<PathBuf> {
    let configured_home = configured_home.map(Path::to_path_buf)?;
    let Some(resume_session_id) = resume_session_id else {
        return Some(configured_home);
    };
    if !looks_like_thread_uuid(resume_session_id) {
        return Some(configured_home);
    }
    if home_contains_rollout(&configured_home, resume_session_id) {
        return Some(configured_home);
    }

    let legacy_home = legacy_codex_home()?;
    if legacy_home != configured_home && home_contains_rollout(&legacy_home, resume_session_id) {
        return Some(legacy_home);
    }

    Some(configured_home)
}
