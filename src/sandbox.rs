use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Context, Result};
use tempfile::TempDir;
const CONTAINER_HOME: &str = "/var/tmp/rust-chatbot-runsc-home";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunscMountTarget {
    SamePath,
    Absolute(PathBuf),
    WorkspaceRelative(PathBuf),
    ContainerHome(PathBuf),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunscMount {
    pub host: PathBuf,
    pub target: RunscMountTarget,
}
impl RunscMount {
    pub fn same_path(path: impl Into<PathBuf>) -> Self {
        Self {
            host: path.into(),
            target: RunscMountTarget::SamePath,
        }
    }

    pub fn absolute(host: impl Into<PathBuf>, container: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            target: RunscMountTarget::Absolute(container.into()),
        }
    }

    pub fn workspace_relative(host: impl Into<PathBuf>, relative: impl AsRef<Path>) -> Self {
        Self {
            host: host.into(),
            target: RunscMountTarget::WorkspaceRelative(relative.as_ref().to_path_buf()),
        }
    }

    pub fn container_home(host: impl Into<PathBuf>, relative: impl AsRef<Path>) -> Self {
        Self {
            host: host.into(),
            target: RunscMountTarget::ContainerHome(relative.as_ref().to_path_buf()),
        }
    }

    fn container_path(&self, workspace_root: &Path) -> PathBuf {
        match &self.target {
            RunscMountTarget::SamePath => self.host.clone(),
            RunscMountTarget::Absolute(path) => path.clone(),
            RunscMountTarget::WorkspaceRelative(path) => workspace_root.join(path),
            RunscMountTarget::ContainerHome(path) => Path::new(CONTAINER_HOME).join(path),
        }
    }
}
pub fn wrap_command_string_with_runsc(
    base_command: String,
    working_dir: Option<&str>,
    runsc_image: Option<&str>,
) -> String {
    let Some(image) = runsc_image.filter(|value| !value.trim().is_empty()) else {
        return base_command;
    };

    let cwd = working_dir
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(".");
    let base_command = rewrite_display_command_for_container(base_command);
    format!(
        "docker run --runtime=runsc --rm --init --workdir {} {} {}",
        cwd, image, base_command
    )
}
fn rewrite_display_command_for_container(base_command: String) -> String {
    let mut parts = base_command.splitn(2, ' ');
    let Some(program) = parts.next() else {
        return base_command;
    };
    let rewritten_program = if Path::new(program).is_absolute() {
        Path::new(program)
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string())
    } else {
        program.to_string()
    };
    match parts.next() {
        Some(rest) => format!("{rewritten_program} {rest}"),
        None => rewritten_program,
    }
}
