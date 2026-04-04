use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use super::{copy_path, remove_existing_path, sync_path_back};

pub(super) fn normalize_workspace_includes(
    includes: &[String],
    source_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut normalized = BTreeSet::new();
    for include in includes {
        let trimmed = include.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            if let Ok(relative) = path.strip_prefix(source_root) {
                let relative = if relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    relative.to_path_buf()
                };
                if relative
                    .components()
                    .any(|c| matches!(c, Component::ParentDir))
                {
                    anyhow::bail!(
                        "runsc workspace include cannot escape the workspace root: {}",
                        relative.display()
                    );
                }
                normalized.insert(relative);
            }
            continue;
        }
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            anyhow::bail!(
                "runsc workspace include cannot escape the workspace root: {}",
                path.display()
            );
        }
        let candidate = if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path.to_path_buf()
        };
        normalized.insert(candidate);
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn effective_workspace_includes(includes: &[PathBuf]) -> Vec<PathBuf> {
    includes.to_vec()
}

fn should_skip_workspace_root_entry(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "target"
                | ".claude-desktop"
                | ".gemini"
                | ".glm"
                | ".minimax"
                | ".nvidia"
                | ".qwen"
                | ".xiaomi"
                | ".gemma"
                | ".rag_model_cache"
                | "tmp"
        )
    )
}

pub(super) fn is_ignorable_rust_incremental_lock(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "lock")
        && path
            .components()
            .map(|component| component.as_os_str())
            .collect::<Vec<_>>()
            .windows(3)
            .any(|window| {
                window[0] == "target" && window[1] == "debug" && window[2] == "incremental"
            })
}

pub(super) fn copy_workspace_root_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)
        .with_context(|| format!("failed to read directory {}", source.display()))?
    {
        let entry = entry?;
        if should_skip_workspace_root_entry(&entry.file_name()) {
            continue;
        }
        copy_path(entry.path(), destination.join(entry.file_name()))?;
    }
    Ok(())
}

pub(super) fn sync_workspace_root_contents(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;

    let source_names: BTreeSet<_> = fs::read_dir(source)
        .with_context(|| format!("failed to read directory {}", source.display()))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("failed to enumerate {}", source.display()))?;

    let destination_names: Vec<_> = fs::read_dir(destination)
        .with_context(|| format!("failed to read directory {}", destination.display()))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("failed to enumerate {}", destination.display()))?;

    for name in destination_names {
        if source_names.contains(&name) || should_skip_workspace_root_entry(&name) {
            continue;
        }
        remove_existing_path(&destination.join(&name))?;
    }

    for name in source_names {
        sync_path_back(&source.join(&name), &destination.join(&name))?;
    }
    Ok(())
}
