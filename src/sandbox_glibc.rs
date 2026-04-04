use std::collections::BTreeSet;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

use super::command_exists;

pub(super) const RUNSC_GLIBC_PROBE_ENTRYPOINT: &str = "/usr/bin/ldd";
static RUNSC_GLIBC_PREFLIGHT_CACHE: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct GlibcVersion {
    pub(super) major: u32,
    pub(super) minor: u32,
}

impl std::fmt::Display for GlibcVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

pub(super) fn ensure_runsc_image_glibc_compatibility(image: &str) -> Result<()> {
    let required = probe_host_glibc_version()?;
    let cache_key = format!("{image}@{required}");
    let cache = RUNSC_GLIBC_PREFLIGHT_CACHE.get_or_init(|| Mutex::new(BTreeSet::new()));
    {
        let cache = cache.lock().expect("runsc glibc preflight cache poisoned");
        if cache.contains(&cache_key) {
            return Ok(());
        }
    }

    let detected = probe_runsc_image_glibc_version(image)?;
    if detected < required {
        anyhow::bail!(
            "runsc image `{image}` reports glibc {detected}, but the mounted host-built binaries require glibc {required}+; rebuild `{image}` on a newer base before launching runsc tasks"
        );
    }

    let mut cache = cache.lock().expect("runsc glibc preflight cache poisoned");
    cache.insert(cache_key);
    Ok(())
}

fn probe_host_glibc_version() -> Result<GlibcVersion> {
    if !command_exists("ldd") {
        anyhow::bail!("runsc sandboxing requires `ldd` on PATH to validate glibc compatibility");
    }
    let output = Command::new("ldd")
        .arg("--version")
        .output()
        .context("failed to execute `ldd --version` for runsc glibc preflight")?;
    if !output.status.success() {
        anyhow::bail!(
            "`ldd --version` failed during runsc glibc preflight: {}",
            format_command_output(&output)
        );
    }
    parse_glibc_version_output(&output.stdout)
        .context("failed to parse host glibc version for runsc preflight")
}

fn probe_runsc_image_glibc_version(image: &str) -> Result<GlibcVersion> {
    let output = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--entrypoint")
        .arg(RUNSC_GLIBC_PROBE_ENTRYPOINT)
        .arg(image)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to probe glibc version for runsc image `{image}`"))?;
    if !output.status.success() {
        anyhow::bail!(
            "runsc image `{image}` failed glibc preflight: {}",
            format_command_output(&output)
        );
    }
    parse_glibc_version_output(&output.stdout)
        .with_context(|| format!("failed to parse glibc version reported by runsc image `{image}`"))
}

fn parse_glibc_version_output(output: &[u8]) -> Result<GlibcVersion> {
    let text = String::from_utf8_lossy(output);
    for line in text.lines() {
        if let Some(version) = parse_glibc_version_line(line) {
            return Ok(version);
        }
    }
    anyhow::bail!(
        "no glibc version found in output `{}`",
        text.lines().next().unwrap_or("<empty>")
    )
}

pub(super) fn parse_glibc_version_line(line: &str) -> Option<GlibcVersion> {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'.' {
            continue;
        }
        let major = line[start..index].parse::<u32>().ok()?;
        index += 1;
        let minor_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if minor_start == index {
            continue;
        }
        let minor = line[minor_start..index].parse::<u32>().ok()?;
        return Some(GlibcVersion { major, minor });
    }
    None
}

fn format_command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("stdout: {stdout}; stderr: {stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => format!("exit status {}", output.status),
    }
}
