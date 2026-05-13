use super::*;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::{Mutex, OnceLock};

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl Into<OsString>) -> Self {
        let previous = env::var_os(key);
        env::set_var(key, value.into());
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

fn env_mutex() -> &'static Mutex<()> {
    static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_MUTEX.get_or_init(|| Mutex::new(()))
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn write_fake_runsc_binaries(bin_dir: &Path, host_glibc: &str, image_glibc: &str) {
    let docker_body = format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "run" ] && [ "${{3:-}}" = "--entrypoint" ] && [ "${{4:-}}" = "{entrypoint}" ]; then
  echo "ldd (Debian GLIBC {image_glibc}-1) {image_glibc}"
  exit 0
fi
exit 0
"#,
        entrypoint = RUNSC_GLIBC_PROBE_ENTRYPOINT,
        image_glibc = image_glibc,
    );
    let ldd_body = format!(
        "#!/bin/sh\nset -eu\necho 'ldd (Ubuntu GLIBC {host_glibc}-1) {host_glibc}'\n",
        host_glibc = host_glibc,
    );
    write_executable(&bin_dir.join("docker"), &docker_body);
    write_executable(&bin_dir.join("runsc"), "#!/bin/sh\nexit 0\n");
    write_executable(&bin_dir.join("ldd"), &ldd_body);
}

#[test]
fn wrap_command_string_without_runsc_is_passthrough() {
    let base = "claude --model claude-opus".to_string();
    assert_eq!(
        wrap_command_string_with_runsc(base.clone(), Some("/tmp/ws"), None),
        base
    );
}

#[test]
fn wrap_command_string_adds_runsc_prefix() {
    let wrapped = wrap_command_string_with_runsc(
        "gemini --model gemini-3-flash-preview".to_string(),
        Some("/tmp/ws"),
        Some("rust-chatbot-autonomous-runsc:latest"),
    );
    assert!(wrapped.contains("docker run --runtime=runsc"));
    assert!(wrapped.contains("--workdir /tmp/ws"));
    assert!(wrapped.contains("rust-chatbot-autonomous-runsc:latest"));
}

#[test]
fn wrap_command_string_rewrites_absolute_program_to_basename() {
    let wrapped = wrap_command_string_with_runsc(
        "/opt/custom/bin/codex exec --json".to_string(),
        Some("/tmp/ws"),
        Some("rust-chatbot-autonomous-runsc:latest"),
    );
    assert!(wrapped.contains("rust-chatbot-autonomous-runsc:latest codex exec --json"));
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_keeps_absolute_program_path() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.39");

    let path_value = format!(
        "{}:{}",
        bin_dir.path().display(),
        env::var("PATH").unwrap_or_default()
    );
    let _path_guard = EnvVarGuard::set("PATH", path_value);

    let workspace = tempfile::tempdir().unwrap();
    let custom_root = tempfile::tempdir().unwrap();
    let custom_cli = custom_root.path().join("bin/codex");
    fs::create_dir_all(custom_cli.parent().unwrap()).unwrap();
    write_executable(&custom_cli, "#!/usr/bin/env bash\nexit 0\n");

    let mut command = Command::new(&custom_cli);
    command.arg("exec").current_dir(workspace.path());

    wrap_command_with_runsc(&mut command, Some("image:latest"), &[], &[]).unwrap();

    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args
        .iter()
        .any(|arg| arg == &custom_cli.display().to_string()));
    assert!(!args.iter().any(|arg| arg == "codex"));
}

#[test]
fn runsc_workspace_sync_copies_selected_paths_back() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "old\n").unwrap();
    fs::write(source.path().join("README.md"), "readme\n").unwrap();

    let sync = stage_runsc_workspace(source.path(), &[String::from("src")]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();
    fs::write(staged_root.join("src/lib.rs"), "new\n").unwrap();
    fs::write(staged_root.join("src/new.rs"), "added\n").unwrap();
    sync.sync_back().unwrap();

    assert_eq!(
        fs::read_to_string(source.path().join("src/lib.rs")).unwrap(),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(source.path().join("src/new.rs")).unwrap(),
        "added\n"
    );
    assert_eq!(
        fs::read_to_string(source.path().join("README.md")).unwrap(),
        "readme\n"
    );
}

#[test]
#[cfg(unix)]
fn stage_runsc_workspace_skips_rust_incremental_lockfiles() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
    fs::write(source.path().join("target-file.txt"), "keep\n").unwrap();
    fs::create_dir_all(source.path().join(".claude-desktop")).unwrap();
    fs::create_dir_all(source.path().join(".git")).unwrap();
    fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        source.path().join(".claude-desktop/session.jsonl"),
        "runtime state\n",
    )
    .unwrap();
    let lock_path = source
        .path()
        .join("target/debug/incremental/crate/s-step.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, []).unwrap();
    let mut permissions = fs::metadata(&lock_path).unwrap().permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&lock_path, permissions).unwrap();

    let sync =
        stage_runsc_workspace(source.path(), &[source.path().display().to_string()]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();

    assert_eq!(
        fs::read_to_string(staged_root.join("src/lib.rs")).unwrap(),
        "pub fn ok() {}\n"
    );
    assert_eq!(
        fs::read_to_string(staged_root.join("target-file.txt")).unwrap(),
        "keep\n"
    );
    assert!(!staged_root.join("target").exists());
    assert!(!staged_root.join(".claude-desktop").exists());
    assert!(!staged_root.join(".git").exists());
}

#[test]
fn runsc_workspace_sync_back_preserves_skipped_root_entries() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::create_dir_all(source.path().join(".git")).unwrap();
    fs::create_dir_all(source.path().join("target/debug")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "old\n").unwrap();
    fs::write(source.path().join("README.md"), "keep\n").unwrap();
    fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(source.path().join("target/debug/app"), "artifact\n").unwrap();

    let sync =
        stage_runsc_workspace(source.path(), &[source.path().display().to_string()]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();
    fs::write(staged_root.join("src/lib.rs"), "new\n").unwrap();
    fs::remove_file(staged_root.join("README.md")).unwrap();

    sync.sync_back().unwrap();

    assert_eq!(
        fs::read_to_string(source.path().join("src/lib.rs")).unwrap(),
        "new\n"
    );
    assert!(!source.path().join("README.md").exists());
    assert_eq!(
        fs::read_to_string(source.path().join("target/debug/app")).unwrap(),
        "artifact\n"
    );
    assert_eq!(
        fs::read_to_string(source.path().join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
}

#[test]
fn parse_glibc_version_line_extracts_major_and_minor() {
    let version = parse_glibc_version_line("ldd (Debian GLIBC 2.41-12+deb13u2) 2.41").unwrap();
    assert_eq!(
        version,
        GlibcVersion {
            major: 2,
            minor: 41
        }
    );
}

#[test]
fn shared_runsc_workspace_disables_sync_back() {
    let _env_lock = env_mutex().lock().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "host\n").unwrap();

    let shared_root = tempfile::tempdir().unwrap();
    fs::create_dir_all(shared_root.path().join("workspace/src")).unwrap();
    fs::write(shared_root.path().join("workspace/src/lib.rs"), "sandbox\n").unwrap();

    let _shared_root_guard = EnvVarGuard::set(RUNSC_SHARED_WORKSPACE_ROOT_ENV, shared_root.path());
    let _disable_sync_guard = EnvVarGuard::set(RUNSC_DISABLE_SYNC_BACK_ENV, "1");

    let sync = stage_runsc_workspace(source.path(), &[String::from("src")]).unwrap();
    assert_eq!(
        sync.staged_root.as_ref(),
        Some(&shared_root.path().join("workspace"))
    );
    sync.sync_back().unwrap();

    assert_eq!(
        fs::read_to_string(source.path().join("src/lib.rs")).unwrap(),
        "host\n"
    );
}

#[test]
fn shared_runsc_workspace_copies_requested_includes_on_first_use() {
    let _env_lock = env_mutex().lock().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "from host\n").unwrap();
    fs::write(source.path().join("README.md"), "hidden\n").unwrap();

    let shared_root = tempfile::tempdir().unwrap();
    let _shared_root_guard = EnvVarGuard::set(RUNSC_SHARED_WORKSPACE_ROOT_ENV, shared_root.path());

    let sync = stage_runsc_workspace(source.path(), &[String::from("src")]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();

    assert_eq!(
        fs::read_to_string(staged_root.join("src/lib.rs")).unwrap(),
        "from host\n"
    );
    assert!(!staged_root.join("README.md").exists());
}

#[test]
fn shared_runsc_workspace_reuses_existing_contents_without_clobbering() {
    let _env_lock = env_mutex().lock().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "host version\n").unwrap();

    let shared_root = tempfile::tempdir().unwrap();
    fs::create_dir_all(shared_root.path().join("workspace/src")).unwrap();
    fs::write(
        shared_root.path().join("workspace/src/lib.rs"),
        "sandbox version\n",
    )
    .unwrap();
    let _shared_root_guard = EnvVarGuard::set(RUNSC_SHARED_WORKSPACE_ROOT_ENV, shared_root.path());

    let sync = stage_runsc_workspace(source.path(), &[String::from("src")]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();

    assert_eq!(
        fs::read_to_string(staged_root.join("src/lib.rs")).unwrap(),
        "sandbox version\n"
    );
}

#[test]
fn shared_runsc_workspace_syncs_back_when_disable_flag_is_absent() {
    let _env_lock = env_mutex().lock().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("src")).unwrap();
    fs::write(source.path().join("src/lib.rs"), "host\n").unwrap();

    let shared_root = tempfile::tempdir().unwrap();
    let _shared_root_guard = EnvVarGuard::set(RUNSC_SHARED_WORKSPACE_ROOT_ENV, shared_root.path());

    let sync = stage_runsc_workspace(source.path(), &[String::from("src")]).unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();
    fs::write(staged_root.join("src/lib.rs"), "sandbox updated\n").unwrap();
    sync.sync_back().unwrap();

    assert_eq!(
        fs::read_to_string(source.path().join("src/lib.rs")).unwrap(),
        "sandbox updated\n"
    );
}

#[test]
fn create_runsc_staging_dir_honors_override_directory() {
    let _env_lock = env_mutex().lock().unwrap();
    let override_dir = tempfile::tempdir().unwrap();
    let _guard = EnvVarGuard::set(RUNSC_STAGING_DIR_ENV, override_dir.path());

    let temp_dir = create_runsc_staging_dir().unwrap();
    assert!(temp_dir.path().starts_with(override_dir.path()));
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_does_not_mount_host_home() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.41");

    let path_value = format!(
        "{}:{}",
        bin_dir.path().display(),
        env::var("PATH").unwrap_or_default()
    );
    let _path_guard = EnvVarGuard::set("PATH", path_value);

    let home_dir = tempfile::tempdir().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", home_dir.path().as_os_str().to_os_string());

    let workspace = tempfile::tempdir().unwrap();
    let mut command = Command::new("echo");
    command.arg("hello").current_dir(workspace.path());

    let _sync = wrap_command_with_runsc(&mut command, Some("image:latest"), &[], &[]).unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let home_mount = format!(
        "{}:{}",
        home_dir.path().display(),
        home_dir.path().display()
    );

    assert!(!args
        .windows(2)
        .any(|pair| pair[0] == "--volume" && pair[1] == home_mount));
    let container_home_mount = format!("{CONTAINER_HOME}:{CONTAINER_HOME}");
    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--volume" && pair[1] == container_home_mount }));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == format!("HOME={CONTAINER_HOME}")));
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_forces_container_home_and_tini_env() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.41");

    let path_value = format!("{}:/host/path/bin:/host/usr/bin", bin_dir.path().display());
    let _path_guard = EnvVarGuard::set("PATH", path_value);
    let _home_guard = EnvVarGuard::set("HOME", "/host/home");

    let _workspace = tempfile::tempdir().unwrap();
    let mut command = Command::new("echo");
    command
        .arg("hello")
        .env("HOME", "/command-home")
        .env("PATH", "/command/bin")
        .env("TINI_SUBREAPER", "0")
        .env("GEMINI_CLI_HOME", "/command-gemini-home")
        .env("GEMINI_FORCE_FILE_STORAGE", "false")
        .env(
            "GEMINI_CLI_PROJECT_SETTINGS_PATH",
            "/command-project-settings",
        )
        .env("RUST_CHATBOT_AUTONOMOUS_INSIDE_RUNSC", "0");

    let _sync = wrap_command_with_runsc(&mut command, Some("image:latest"), &[], &[]).unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let expected_host_path = format!("{}:/host/path/bin:/host/usr/bin", bin_dir.path().display());
    let expected_path = format!("PATH={CONTAINER_PATH}:{expected_host_path}");
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == format!("HOME={CONTAINER_HOME}")));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == expected_path));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == TINI_SUBREAPER_ENV));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == GEMINI_CLI_HOME_ENV));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == GEMINI_FORCE_FILE_STORAGE_ENV));
    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--env" && pair[1] == GEMINI_CLI_PROJECT_SETTINGS_PATH_ENV }));
    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--env" && pair[1] == "RUST_CHATBOT_AUTONOMOUS_INSIDE_RUNSC=1" }));

    assert!(!args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == "HOME=/command-home"));
    assert!(!args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == "PATH=/command/bin"));
    assert!(!args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == "TINI_SUBREAPER=0"));
    assert!(!args
        .windows(2)
        .any(|pair| pair[0] == "--env" && pair[1] == "GEMINI_CLI_HOME=/command-gemini-home"));
    assert!(!args
        .windows(2)
        .any(|pair| { pair[0] == "--env" && pair[1] == "RUST_CHATBOT_AUTONOMOUS_INSIDE_RUNSC=0" }));
    assert!(!args.windows(2).any(|pair| {
        pair[0] == "--env"
            && pair[1] == "GEMINI_CLI_PROJECT_SETTINGS_PATH=/command-project-settings"
    }));
    assert!(!args
        .windows(2)
        .any(|pair| { pair[0] == "--env" && pair[1] == "GEMINI_FORCE_FILE_STORAGE=false" }));
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_mounts_explicit_provider_config_paths() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.41");

    let path_value = format!(
        "{}:{}",
        bin_dir.path().display(),
        env::var("PATH").unwrap_or_default()
    );
    let _path_guard = EnvVarGuard::set("PATH", path_value);

    let workspace = tempfile::tempdir().unwrap();
    let mut command = Command::new("echo");
    command.arg("hello").current_dir(workspace.path());
    let gemini_host = workspace.path().join("gemini-settings.json");
    let qwen_host = workspace.path().join("qwen.json");
    fs::write(&gemini_host, "{}\n").unwrap();
    fs::write(&qwen_host, "{}\n").unwrap();
    let mounts = vec![
        RunscMount::container_home(&gemini_host, ".gemini/settings.json"),
        RunscMount::workspace_relative(&qwen_host, ".qwen/.qwen.json"),
    ];

    let _sync = wrap_command_with_runsc(&mut command, Some("image:latest"), &[], &mounts).unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let gemini_expected = format!(
        "{}:{}",
        gemini_host.display(),
        Path::new(CONTAINER_HOME)
            .join(".gemini/settings.json")
            .display()
    );
    let qwen_prefix = format!("{}:", qwen_host.display());
    let qwen_suffix = "/.qwen/.qwen.json";
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--volume" && pair[1] == gemini_expected));
    assert!(args.windows(2).any(|pair| {
        pair[0] == "--volume" && pair[1].starts_with(&qwen_prefix) && pair[1].ends_with(qwen_suffix)
    }));
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_rewrites_workspace_scoped_path_args() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.41");

    let path_value = format!(
        "{}:{}",
        bin_dir.path().display(),
        env::var("PATH").unwrap_or_default()
    );
    let _path_guard = EnvVarGuard::set("PATH", path_value);

    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("instructions")).unwrap();
    fs::write(workspace.path().join("plan.txt"), "plan\n").unwrap();
    let mut command = Command::new("gemini");
    command
        .arg("--include-directories")
        .arg(workspace.path())
        .arg("--workspace-root")
        .arg(workspace.path().join("instructions"))
        .current_dir(workspace.path());

    let sync = wrap_command_with_runsc(
        &mut command,
        Some("image:latest"),
        &["instructions".to_string(), "plan.txt".to_string()],
        &[],
    )
    .unwrap();
    let staged_root = sync.staged_root.as_ref().unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args.windows(2).any(|pair| {
        pair[0] == "--include-directories" && pair[1] == staged_root.display().to_string()
    }));
    assert!(args.windows(2).any(|pair| {
        pair[0] == "--workspace-root"
            && pair[1] == staged_root.join("instructions").display().to_string()
    }));
}

#[test]
fn program_mounts_skip_standard_system_bin_dirs() {
    let mounts = program_mounts(&OsString::from("/usr/bin/python3"));
    assert!(mounts.is_empty());
}

#[test]
fn program_mounts_keep_nonstandard_install_roots() {
    let root = tempfile::tempdir().unwrap();
    let bin_dir = root.path().join("bin");
    let lib_dir = root.path().join("lib");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&lib_dir).unwrap();
    let target = lib_dir.join("tool");
    write_executable(&target, "#!/usr/bin/env bash\nexit 0\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, bin_dir.join("tool")).unwrap();

    let mounts = program_mounts(&bin_dir.join("tool").as_os_str().to_os_string());
    assert_eq!(mounts, vec![root.path().to_path_buf()]);
}

#[test]
#[cfg(target_os = "linux")]
fn wrap_command_with_runsc_rejects_image_with_too_old_glibc() {
    let _env_lock = env_mutex().lock().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_fake_runsc_binaries(bin_dir.path(), "2.39", "2.36");

    let path_value = format!(
        "{}:{}",
        bin_dir.path().display(),
        env::var("PATH").unwrap_or_default()
    );
    let _path_guard = EnvVarGuard::set("PATH", path_value);

    let workspace = tempfile::tempdir().unwrap();
    let mut command = Command::new("echo");
    command.arg("hello").current_dir(workspace.path());

    let err =
        wrap_command_with_runsc(&mut command, Some("image-too-old:latest"), &[], &[]).unwrap_err();
    assert!(err
        .to_string()
        .contains("glibc 2.36, but the mounted host-built binaries require glibc 2.39+"));
}

#[test]
#[cfg(unix)]
fn program_mounts_preserve_directory_aliases_in_symlink_targets() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    fs::create_dir_all(real.join("bin")).unwrap();
    fs::create_dir_all(real.join("lib")).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let tool = alias.join("lib/tool");
    write_executable(&tool, "#!/bin/sh\nexit 0\n");
    std::os::unix::fs::symlink(&tool, alias.join("bin/tool")).unwrap();
    let mounts = program_mounts(&alias.join("bin/tool").into_os_string());
    assert_eq!(mounts, vec![alias]);
}

#[test]
#[cfg(unix)]
fn program_mounts_follow_relative_symlink_chains() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(root.path().join("lib")).unwrap();
    write_executable(&root.path().join("lib/tool"), "#!/bin/sh\nexit 0\n");
    std::os::unix::fs::symlink("hop", bin.join("tool")).unwrap();
    std::os::unix::fs::symlink("../lib/tool", bin.join("hop")).unwrap();
    assert_eq!(
        program_mounts(&bin.join("tool").into_os_string()),
        vec![root.path().to_path_buf()]
    );
}
