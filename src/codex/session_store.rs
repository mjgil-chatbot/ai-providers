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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ai-providers-codex-{name}-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_rollout(home: &Path, session_id: &str) {
        let rollout_dir = home.join("sessions").join("2026").join("05").join("03");
        std::fs::create_dir_all(&rollout_dir).unwrap();
        std::fs::write(
            rollout_dir.join(format!("rollout-2026-05-03T00-00-00-{session_id}.jsonl")),
            "",
        )
        .unwrap();
    }

    #[test]
    fn select_codex_home_keeps_configured_home_without_resume_id() {
        let configured = unique_temp_dir("configured-default");
        let selected = select_codex_home_for_resume(Some(configured.as_path()), None).unwrap();
        assert_eq!(selected, configured);
    }

    #[test]
    fn select_codex_home_keeps_configured_home_when_rollout_exists_there() {
        let configured = unique_temp_dir("configured-hit");
        let session_id = "019dec06-caf0-7f83-9e5f-ac761eb4d03b";
        write_rollout(&configured, session_id);

        let selected =
            select_codex_home_for_resume(Some(configured.as_path()), Some(session_id)).unwrap();
        assert_eq!(selected, configured);
    }

    #[test]
    fn select_codex_home_uses_legacy_home_when_configured_home_misses_rollout() {
        let previous_home = std::env::var_os("HOME");
        let temp_home = unique_temp_dir("legacy-home");
        let configured = temp_home.join(".rust-chatbot").join(".codex");
        std::fs::create_dir_all(&configured).unwrap();
        let legacy = temp_home.join(".codex");
        let session_id = "019dec06-caf0-7f83-9e5f-ac761eb4d03b";
        write_rollout(&legacy, session_id);
        std::env::set_var("HOME", &temp_home);

        let selected =
            select_codex_home_for_resume(Some(configured.as_path()), Some(session_id)).unwrap();

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(selected, legacy);
    }

    #[test]
    fn select_codex_home_does_not_switch_non_uuid_handles() {
        let previous_home = std::env::var_os("HOME");
        let temp_home = unique_temp_dir("legacy-name");
        let configured = temp_home.join(".rust-chatbot").join(".codex");
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::create_dir_all(temp_home.join(".codex")).unwrap();
        std::env::set_var("HOME", &temp_home);

        let selected = select_codex_home_for_resume(
            Some(configured.as_path()),
            Some("chatbot-cli-2 (b274f15f)"),
        )
        .unwrap();

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(selected, configured);
    }
}
