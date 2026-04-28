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
