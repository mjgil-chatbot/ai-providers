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
