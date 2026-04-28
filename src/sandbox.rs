use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::TempDir;

#[path = "sandbox_glibc.rs"]
mod sandbox_glibc;
#[path = "sandbox_workspace.rs"]
mod sandbox_workspace;
use sandbox_glibc::ensure_runsc_image_glibc_compatibility;
#[cfg(test)]
use sandbox_glibc::{parse_glibc_version_line, GlibcVersion, RUNSC_GLIBC_PROBE_ENTRYPOINT};
use sandbox_workspace::{
    copy_workspace_root_contents, effective_workspace_includes, is_ignorable_rust_incremental_lock,
    normalize_workspace_includes, sync_workspace_root_contents,
};

const RUNSC_REQUIRED_COMMANDS: &[&str] = &["docker", "runsc"];
const RUNSC_WORKSPACE_PATH_ARG_FLAGS: &[&str] = &["--include-directories", "--workspace-root"];
const RUNSC_STAGING_DIR_ENV: &str = "RUST_CHATBOT_RUNSC_STAGING_DIR";
const RUNSC_SHARED_WORKSPACE_ROOT_ENV: &str = "RUST_CHATBOT_AUTONOMOUS_RUNSC_SHARED_ROOT";
const RUNSC_DISABLE_SYNC_BACK_ENV: &str = "RUST_CHATBOT_AUTONOMOUS_RUNSC_DISABLE_SYNC_BACK";
const HOST_ENV_PASSTHROUGH_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "ASDF_",
    "CODEX_",
    "GEMINI_",
    "GLM_",
    "GOOGLE_",
    "MINIMAX_",
    "NPM_",
    "NODE_",
    "OPENAI_",
    "QWEN_",
    "RUST_CHATBOT_",
];
const HOST_ENV_PASSTHROUGH_NAMES: &[&str] = &[
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "PATH",
    "SHELL",
    "SSH_AUTH_SOCK",
    "TERM",
    "TMPDIR",
    "USER",
];
const CONTAINER_HOME: &str = "/var/tmp/rust-chatbot-runsc-home";
const CONTAINER_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const TINI_SUBREAPER_ENV: &str = "TINI_SUBREAPER=1";
const GEMINI_CLI_HOME_ENV: &str = "GEMINI_CLI_HOME=/var/tmp/rust-chatbot-runsc-home";
const GEMINI_CLI_TRUST_WORKSPACE_ENV: &str = "GEMINI_CLI_TRUST_WORKSPACE=true";
const GEMINI_FORCE_FILE_STORAGE_ENV: &str = "GEMINI_FORCE_FILE_STORAGE=true";
const GEMINI_CLI_PROJECT_SETTINGS_PATH_ENV: &str =
    "GEMINI_CLI_PROJECT_SETTINGS_PATH=/var/tmp/rust-chatbot-runsc-home/.gemini/settings.json";
const RUNSC_FORCED_ENV_NAMES: &[&str] = &[
    "HOME",
    "PATH",
    "RUST_CHATBOT_AUTONOMOUS_INSIDE_RUNSC",
    "TINI_SUBREAPER",
    "GEMINI_CLI_HOME",
    "GEMINI_CLI_TRUST_WORKSPACE",
    "GEMINI_FORCE_FILE_STORAGE",
    "GEMINI_CLI_PROJECT_SETTINGS_PATH",
];

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

#[derive(Debug)]
pub struct RunscWorkspaceSync {
    _temp_dir: Option<TempDir>,
    source_root: Option<PathBuf>,
    staged_root: Option<PathBuf>,
    includes: Vec<PathBuf>,
    sync_back_enabled: bool,
}

impl RunscWorkspaceSync {
    pub fn noop() -> Self {
        Self {
            _temp_dir: None,
            source_root: None,
            staged_root: None,
            includes: Vec::new(),
            sync_back_enabled: true,
        }
    }

    pub fn staged_root(&self) -> Option<&std::path::Path> {
        self.staged_root.as_deref()
    }

    pub fn sync_back(&self) -> Result<()> {
        if !self.sync_back_enabled {
            return Ok(());
        }
        let (Some(source_root), Some(staged_root)) =
            (self.source_root.as_ref(), self.staged_root.as_ref())
        else {
            return Ok(());
        };

        let includes = effective_workspace_includes(&self.includes);
        for include in includes {
            if include == Path::new(".") {
                sync_workspace_root_contents(staged_root, source_root)?;
            } else {
                sync_path_back(&staged_root.join(&include), &source_root.join(&include))?;
            }
        }
        Ok(())
    }
}

pub fn wrap_command_with_runsc(
    command: &mut Command,
    runsc_image: Option<&str>,
    runsc_workspace_includes: &[String],
    runsc_mounts: &[RunscMount],
) -> Result<RunscWorkspaceSync> {
    let Some(image) = runsc_image
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(RunscWorkspaceSync::noop());
    };

    if !cfg!(target_os = "linux") {
        anyhow::bail!("runsc sandboxing is only supported on Linux");
    }

    for required in RUNSC_REQUIRED_COMMANDS {
        if !command_exists(required) {
            anyhow::bail!("runsc sandboxing requires `{required}` on PATH");
        }
    }
    ensure_runsc_image_glibc_compatibility(&image)?;

    let original_program = command.get_program().to_os_string();
    let container_program = resolve_container_program(&original_program);
    let original_args: Vec<OsString> = command.get_args().map(OsString::from).collect();
    let original_envs: Vec<(OsString, OsString)> = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect();
    let source_current_dir = command
        .get_current_dir()
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let workspace_sync = stage_runsc_workspace(&source_current_dir, runsc_workspace_includes)?;
    let current_dir = workspace_sync
        .staged_root
        .as_ref()
        .cloned()
        .unwrap_or_else(|| source_current_dir.clone());
    command.current_dir(&current_dir);
    let rewritten_args =
        rewrite_workspace_arg_paths(&original_args, &source_current_dir, &current_dir);

    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let host_path = env::var("PATH").unwrap_or_else(|_| CONTAINER_PATH.to_string());
    fs::create_dir_all(CONTAINER_HOME)
        .with_context(|| format!("failed to create container home at {CONTAINER_HOME}"))?;
    // Gemini CLI and other tools store config in ~/.gemini, ~/.claude, etc.
    // Ensure these directories exist when HOME is redirected to CONTAINER_HOME.
    let dotgemini = Path::new(CONTAINER_HOME).join(".gemini");
    fs::create_dir_all(&dotgemini).with_context(|| {
        format!(
            "failed to create gemini config directory at {}",
            dotgemini.display()
        )
    })?;

    let mut mounts = BTreeSet::new();
    mounts.insert(PathBuf::from(CONTAINER_HOME));
    mounts.insert(PathBuf::from("/tmp"));
    mounts.insert(current_dir.clone());
    for program_path in program_mounts(&original_program) {
        mounts.insert(program_path);
    }
    for (_, value) in &original_envs {
        maybe_add_env_mount(&mut mounts, value);
    }

    let mut wrapped = Command::new("docker");
    wrapped
        .arg("run")
        .arg("--runtime=runsc")
        .arg("--rm")
        .arg("--init")
        .arg("--user")
        .arg(format!("{uid}:{gid}"))
        .arg("--workdir")
        .arg(&current_dir);

    let mut bind_specs = BTreeSet::new();
    for mount in mounts {
        if mount.exists() {
            bind_specs.insert(format!("{}:{}", mount.display(), mount.display()));
        }
    }
    for mount in runsc_mounts {
        if !mount.host.exists() {
            continue;
        }
        let container = mount.container_path(&current_dir);
        prepare_mount_target(&container, &mount.host)?;
        bind_specs.insert(format!("{}:{}", mount.host.display(), container.display()));
    }
    for bind_spec in bind_specs {
        wrapped.arg("--volume").arg(bind_spec);
    }
    // Explicitly ensure CONTAINER_HOME is mounted so ~/.gemini and other config
    // directories are accessible when HOME is redirected to CONTAINER_HOME.
    if Path::new(CONTAINER_HOME).exists() {
        wrapped.arg("--volume");
        wrapped.arg(format!("{}:{}", CONTAINER_HOME, CONTAINER_HOME));
    }

    for name in HOST_ENV_PASSTHROUGH_NAMES {
        if RUNSC_FORCED_ENV_NAMES.contains(&name) {
            continue;
        }
        if let Ok(value) = env::var(name) {
            wrapped.arg("--env").arg(format!("{name}={value}"));
        }
    }
    for (name, value) in env::vars() {
        if RUNSC_FORCED_ENV_NAMES.contains(&name.as_str()) {
            continue;
        }
        if HOST_ENV_PASSTHROUGH_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            wrapped.arg("--env").arg(format!("{name}={value}"));
        }
    }
    for (key, value) in original_envs {
        let key = key.to_string_lossy();
        if RUNSC_FORCED_ENV_NAMES.contains(&key.as_ref()) {
            continue;
        }
        wrapped
            .arg("--env")
            .arg(format!("{}={}", key, value.to_string_lossy()));
    }
    wrapped.arg("--env").arg(format!("HOME={CONTAINER_HOME}"));
    wrapped
        .arg("--env")
        .arg(format!("PATH={CONTAINER_PATH}:{host_path}"));
    wrapped
        .arg("--env")
        .arg("RUST_CHATBOT_AUTONOMOUS_INSIDE_RUNSC=1");
    wrapped.arg("--env").arg(TINI_SUBREAPER_ENV);
    wrapped
        .arg("--env")
        .arg(GEMINI_CLI_HOME_ENV)
        .arg("--env")
        .arg(GEMINI_CLI_TRUST_WORKSPACE_ENV)
        .arg("--env")
        .arg(GEMINI_FORCE_FILE_STORAGE_ENV)
        .arg("--env")
        .arg(GEMINI_CLI_PROJECT_SETTINGS_PATH_ENV);
    wrapped.arg(&image);
    wrapped.arg(&container_program);
    wrapped.args(rewritten_args);

    *command = wrapped;
    Ok(workspace_sync)
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

fn command_exists(command: &str) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

fn stage_runsc_workspace(source_root: &Path, includes: &[String]) -> Result<RunscWorkspaceSync> {
    let includes = normalize_workspace_includes(includes, source_root)?;
    let effective_includes = effective_workspace_includes(&includes);
    let sync_back_enabled = !env_var_enabled(RUNSC_DISABLE_SYNC_BACK_ENV);
    if let Some(shared_root) = shared_runsc_workspace_root() {
        let staged_root =
            prepare_shared_runsc_workspace(&shared_root, source_root, &effective_includes)?;
        return Ok(RunscWorkspaceSync {
            _temp_dir: None,
            source_root: Some(source_root.to_path_buf()),
            staged_root: Some(staged_root),
            includes,
            sync_back_enabled,
        });
    }
    let temp_dir = create_runsc_staging_dir()?;
    let staged_root = temp_dir.path().join("workspace");
    fs::create_dir_all(&staged_root).with_context(|| {
        format!(
            "failed to create runsc staged workspace root at {}",
            staged_root.display()
        )
    })?;

    for include in &effective_includes {
        if include == Path::new(".") {
            copy_workspace_root_contents(source_root, &staged_root)?;
        } else {
            copy_path(source_root.join(include), staged_root.join(include))?;
        }
    }

    Ok(RunscWorkspaceSync {
        _temp_dir: Some(temp_dir),
        source_root: Some(source_root.to_path_buf()),
        staged_root: Some(staged_root),
        includes,
        sync_back_enabled,
    })
}

fn create_runsc_staging_dir() -> Result<TempDir> {
    if let Some(dir) = env::var_os(RUNSC_STAGING_DIR_ENV).filter(|value| !value.is_empty()) {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).with_context(|| {
            format!(
                "failed to create runsc staging directory override at {}",
                dir.display()
            )
        })?;
        return tempfile::Builder::new()
            .prefix("rust-chatbot-runsc.")
            .tempdir_in(&dir)
            .with_context(|| {
                format!(
                    "failed to create runsc staging directory in {}",
                    dir.display()
                )
            });
    }

    let preferred_dir = Path::new("/var/tmp");
    if preferred_dir.is_dir() {
        return tempfile::Builder::new()
            .prefix("rust-chatbot-runsc.")
            .tempdir_in(preferred_dir)
            .context("failed to create runsc staging directory in /var/tmp");
    }

    tempfile::Builder::new()
        .prefix("rust-chatbot-runsc.")
        .tempdir()
        .context("failed to create runsc staging directory")
}

fn env_var_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn shared_runsc_workspace_root() -> Option<PathBuf> {
    std::env::var_os(RUNSC_SHARED_WORKSPACE_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn prepare_shared_runsc_workspace(
    shared_root: &Path,
    source_root: &Path,
    effective_includes: &[PathBuf],
) -> Result<PathBuf> {
    let staged_root = shared_root.join("workspace");
    fs::create_dir_all(&staged_root)
        .with_context(|| format!("failed to create {}", staged_root.display()))?;

    let is_empty = fs::read_dir(&staged_root)
        .with_context(|| format!("failed to read {}", staged_root.display()))?
        .next()
        .is_none();
    if is_empty {
        for include in effective_includes {
            if include == Path::new(".") {
                copy_workspace_root_contents(source_root, &staged_root)?;
            } else {
                copy_path(source_root.join(include), staged_root.join(include))?;
            }
        }
    }

    Ok(staged_root)
}

pub fn program_directory_mounts(command: &str) -> Vec<PathBuf> {
    program_mounts(&OsString::from(command))
}

fn program_mounts(program: &OsString) -> Vec<PathBuf> {
    let path = Path::new(program);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(p) = resolve_program_in_path(program) {
        p
    } else {
        return Vec::new();
    };

    let symlink_parent = match resolved.parent() {
        Some(p) => p.to_path_buf(),
        None => return Vec::new(),
    };

    // If the binary is a symlink, find the common ancestor of the symlink
    // location and the real target so that the entire installation tree
    // (bin/, lib/node_modules/, etc.) is covered by one mount.
    if let Some(target) = follow_program_symlinks(&resolved) {
        // Follow the link as written. Canonicalizing the entire path also
        // resolves directory aliases (for example /var -> /private/var),
        // destroying the shared installation prefix used by the link.
        if let Some(target_parent) = target.parent() {
            if let Some(ancestor) = common_path_ancestor(&symlink_parent, target_parent) {
                if should_skip_program_mount(&ancestor) {
                    return Vec::new();
                }
                return vec![ancestor];
            }
        }
    }

    if should_skip_program_mount(&symlink_parent) {
        return Vec::new();
    }

    vec![symlink_parent]
}

fn follow_program_symlinks(path: &Path) -> Option<PathBuf> {
    let mut path = path.to_path_buf();
    let mut followed = false;
    for _ in 0..40 {
        match fs::read_link(&path) {
            Ok(target) => {
                path = lexical_symlink_target(path.parent()?, target);
                followed = true;
            }
            Err(_) => return followed.then_some(path),
        }
    }
    None
}

fn lexical_symlink_target(parent: &Path, target: PathBuf) -> PathBuf {
    let target = if target.is_absolute() {
        target
    } else {
        parent.join(target)
    };
    let mut normalized = PathBuf::new();
    for component in target.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn should_skip_program_mount(path: &Path) -> bool {
    matches!(
        path,
        p if p == Path::new("/")
            || p == Path::new("/bin")
            || p == Path::new("/sbin")
            || p == Path::new("/usr")
            || p == Path::new("/usr/bin")
            || p == Path::new("/usr/sbin")
            || p == Path::new("/usr/local")
            || p == Path::new("/usr/local/bin")
            || p == Path::new("/usr/local/sbin")
    )
}

fn common_path_ancestor(a: &Path, b: &Path) -> Option<PathBuf> {
    let mut common: Vec<std::path::Component> = Vec::new();
    for (ca, cb) in a.components().zip(b.components()) {
        if ca == cb {
            common.push(ca);
        } else {
            break;
        }
    }
    if common.is_empty() {
        return None;
    }
    Some(common.iter().collect())
}

fn resolve_program_in_path(program: &OsString) -> Option<PathBuf> {
    let name = Path::new(program);
    // Only resolve simple names, not paths with separators.
    if name.components().count() != 1 {
        return None;
    }
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_container_program(program: &OsString) -> OsString {
    let path = Path::new(program);
    if path.is_absolute() {
        return program.clone();
    }
    program.clone()
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

fn rewrite_workspace_arg_paths(
    args: &[OsString],
    source_root: &Path,
    staged_root: &Path,
) -> Vec<OsString> {
    let mut rewritten = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        rewritten.push(arg.clone());
        let flag = arg.to_string_lossy();
        if RUNSC_WORKSPACE_PATH_ARG_FLAGS
            .iter()
            .any(|candidate| *candidate == flag)
        {
            if let Some(value) = args.get(index + 1) {
                rewritten.push(rewrite_workspace_path_value(
                    value,
                    source_root,
                    staged_root,
                ));
                index += 2;
                continue;
            }
        }
        index += 1;
    }
    rewritten
}

fn rewrite_workspace_path_value(
    value: &OsString,
    source_root: &Path,
    staged_root: &Path,
) -> OsString {
    let path = Path::new(value);
    if path == source_root {
        return staged_root.as_os_str().to_os_string();
    }
    if let Ok(relative) = path.strip_prefix(source_root) {
        return staged_root.join(relative).into_os_string();
    }
    value.clone()
}

fn maybe_add_env_mount(mounts: &mut BTreeSet<PathBuf>, value: &OsString) {
    let path = Path::new(value);
    if !path.is_absolute() {
        return;
    }
    let candidate = if path.is_dir() {
        Some(path.to_path_buf())
    } else if path.is_file() {
        path.parent().map(PathBuf::from)
    } else {
        None
    };
    if let Some(candidate) = candidate {
        mounts.insert(candidate);
    }
}

fn prepare_mount_target(container: &Path, host: &Path) -> Result<()> {
    let metadata = fs::metadata(host)
        .with_context(|| format!("failed to stat mount source {}", host.display()))?;
    if let Some(parent) = container.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if metadata.is_dir() {
        fs::create_dir_all(container)
            .with_context(|| format!("failed to create {}", container.display()))?;
    } else if !container.exists() {
        fs::write(container, [])
            .with_context(|| format!("failed to create {}", container.display()))?;
    }
    Ok(())
}

fn copy_path(source: PathBuf, destination: PathBuf) -> Result<()> {
    if is_ignorable_rust_incremental_lock(&source) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(&source)
        .with_context(|| format!("failed to stat {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        copy_symlink(&source, &destination)?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(&destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        for entry in fs::read_dir(&source)
            .with_context(|| format!("failed to read directory {}", source.display()))?
        {
            let entry = entry?;
            copy_path(entry.path(), destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(&source, &destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    fs::set_permissions(&destination, metadata.permissions())
        .with_context(|| format!("failed to set permissions on {}", destination.display()))?;
    Ok(())
}

fn sync_path_back(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() && fs::symlink_metadata(source).is_err() {
        if destination.exists() || fs::symlink_metadata(destination).is_ok() {
            remove_existing_path(destination)?;
        }
        return Ok(());
    }

    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        remove_existing_path(destination).ok();
        copy_symlink(source, destination)?;
        return Ok(());
    }
    if metadata.is_dir() {
        if destination.exists() || fs::symlink_metadata(destination).is_ok() {
            remove_existing_path(destination)?;
        }
        copy_path(source.to_path_buf(), destination.to_path_buf())?;
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    remove_existing_path(destination).ok();
    copy_path(source.to_path_buf(), destination.to_path_buf())
}

fn remove_existing_path(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove file {}", path.display()))?;
    }
    Ok(())
}

fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)
        .with_context(|| format!("failed to read symlink {}", source.display()))?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    remove_existing_path(destination).ok();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, destination).with_context(|| {
            format!(
                "failed to create symlink {} -> {}",
                destination.display(),
                target.display()
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("runsc workspace symlink staging is only supported on unix");
    }
    Ok(())
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
