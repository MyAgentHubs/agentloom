use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub(super) enum LookupStrategy {
    WindowsPathScan,
    UnixWhich,
}

pub(super) fn lookup_strategy(windows: bool) -> LookupStrategy {
    if windows {
        LookupStrategy::WindowsPathScan
    } else {
        LookupStrategy::UnixWhich
    }
}

pub(super) fn lookup_command(bin: &str, augmented_path: Option<OsString>) -> std::process::Command {
    let mut cmd = crate::proc::command("which");
    cmd.arg(bin);
    if let Some(path) = augmented_path {
        cmd.env("PATH", path);
    }
    cmd
}

pub(super) fn windows_executable_on_path_from(
    bin: &str,
    augmented_path: Option<&OsStr>,
    process_path: Option<&OsStr>,
    path_exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let search_path = augmented_path.or(process_path)?;
    find_windows_executable_in_dirs(std::env::split_paths(search_path), bin, path_exists)
}

pub(super) fn find_windows_executable_in_dirs<I, P>(
    directories: I,
    bin: &str,
    mut path_exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let executables = windows_executable_candidates(bin);
    for directory in directories {
        let directory = directory.as_ref();
        if !windows_path_is_absolute(directory) {
            continue;
        }
        for executable in &executables {
            let candidate = directory.join(executable);
            if path_exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn windows_path_is_absolute(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let Some(path) = path.to_str() else {
        return false;
    };
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with(r"\\")
}

/// Extract a named value from `reg query ... /v <name>` output.
pub(super) fn parse_reg_query_value(stdout: &str, value_name: &str) -> Option<String> {
    for line in stdout.lines() {
        for value_type in ["REG_EXPAND_SZ", "REG_SZ"] {
            let Some(type_start) = line.find(value_type) else {
                continue;
            };
            let name = line[..type_start].trim();
            let value = line[type_start + value_type.len()..].trim();
            if name.eq_ignore_ascii_case(value_name) && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Expand Windows-style `%NAME%` environment references, preserving unknown ones.
pub(super) fn expand_windows_env_refs(
    value: &str,
    env: impl Fn(&str) -> Option<OsString>,
) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(open) = remaining.find('%') {
        expanded.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('%') else {
            expanded.push_str(&remaining[open..]);
            return expanded;
        };
        let name = &after_open[..close];
        if name.is_empty() {
            expanded.push_str("%%");
        } else if let Some(replacement) = env(name) {
            expanded.push_str(&replacement.to_string_lossy());
        } else {
            expanded.push('%');
            expanded.push_str(name);
            expanded.push('%');
        }
        remaining = &after_open[close + 1..];
    }
    expanded.push_str(remaining);
    expanded
}

pub(super) const MACHINE_ENVIRONMENT_KEY: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
pub(super) const USER_ENVIRONMENT_KEY: &str = r"HKCU\Environment";

pub(super) fn split_trusted_registry_path_entries(path: &str) -> Vec<&str> {
    path.split(';')
        .map(str::trim)
        .filter(|entry| {
            // reg.exe writes redirected output in the active OEM code page, not necessarily
            // UTF-8. `from_utf8_lossy` marks undecodable bytes with U+FFFD, so such an entry is
            // not trustworthy enough to turn into a filesystem candidate.
            !entry.is_empty() && !entry.contains('\u{FFFD}')
        })
        .collect()
}

/// Build the effective registry PATH: machine entries first, then user entries.
pub(super) fn registry_path_dirs(
    run_reg: impl Fn(&str) -> Option<String>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut directories = Vec::new();
    for key in [MACHINE_ENVIRONMENT_KEY, USER_ENVIRONMENT_KEY] {
        let Some(stdout) = run_reg(key) else {
            continue;
        };
        let Some(path) = parse_reg_query_value(&stdout, "Path") else {
            continue;
        };
        let expanded = expand_windows_env_refs(&path, &env);
        for directory in split_trusted_registry_path_entries(&expanded) {
            let directory = PathBuf::from(directory);
            if windows_path_is_absolute(&directory)
                && seen.insert(directory.to_string_lossy().to_lowercase())
            {
                directories.push(directory);
            }
        }
    }
    directories
}

pub(super) fn windows_executable_candidates(bin: &str) -> Vec<OsString> {
    // Rust >= 1.77.2's std/sys/process/windows.rs uses is_batch_file
    // to recognize .cmd/.bat case-insensitively, and Command automatically routes them through cmd.exe;
    // .ps1/.vbs have no such handling layer, and CreateProcess cannot launch them directly,
    // so only exe/cmd/bat are accepted here.
    if Path::new(bin)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(windows_executable_extension_allowed)
    {
        vec![OsString::from(bin)]
    } else {
        ["exe", "cmd", "bat"]
            .map(|extension| OsString::from(format!("{bin}.{extension}")))
            .into()
    }
}

pub(super) fn windows_executable_extension_allowed(extension: &str) -> bool {
    ["exe", "cmd", "bat"]
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

pub(super) fn windows_fallbacks_from(
    bin: &str,
    env: impl Fn(&str) -> Option<OsString>,
    list_subdirectories: impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) -> Vec<PathBuf> {
    let executables = windows_executable_candidates(bin);
    let home = env("HOME");
    let user_profile = env("USERPROFILE");
    let homes = super::home_dirs_from(home.as_deref(), user_profile.as_deref(), true);
    let local_app_data = env("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let app_data = env("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let program_files = env("ProgramFiles")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let program_data = env("ProgramData")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    // Keep both roots for each home-relative location adjacent. This preserves the
    // existing directory priority while trying HOME before USERPROFILE at each slot.
    let mut directories = Vec::new();
    directories.extend(homes.iter().map(|root| root.join(".local").join("bin")));
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Microsoft").join("WinGet").join("Links")),
    );
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Microsoft").join("WindowsApps")),
    );
    if let Some(local_app_data) = &local_app_data {
        let packages = local_app_data
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        let mut package_directories = list_subdirectories(&packages).unwrap_or_default();
        package_directories.sort();
        directories.extend(package_directories);
    }
    directories.extend(app_data.as_ref().map(|root| root.join("npm")));
    directories.extend(program_files.as_ref().map(|root| root.join("nodejs")));
    directories.extend(local_app_data.as_ref().map(|root| root.join("pnpm")));
    directories.extend(homes.iter().map(|root| root.join(".bun").join("bin")));
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Volta").join("bin")),
    );
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Yarn").join("bin")),
    );
    directories.extend(homes.iter().map(|root| root.join("scoop").join("shims")));
    directories.extend(
        program_data
            .as_ref()
            .map(|root| root.join("chocolatey").join("bin")),
    );

    let mut candidates = Vec::new();
    for directory in directories {
        candidates.extend(
            executables
                .iter()
                .map(|executable| directory.join(executable)),
        );
    }
    candidates
}
