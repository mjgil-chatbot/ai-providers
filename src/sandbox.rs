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
