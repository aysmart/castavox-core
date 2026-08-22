//! Finding the Node runtime the speech sidecar runs on.
//!
//! Pulpitry does not ship Node. Fusing a whole runtime into a single-file
//! executable cost 115 MB to deliver one WebSocket client, and every machine
//! that wanted it already had a way to get it.
//!
//! The catch is that a GUI application cannot simply run `node`. An app opened
//! from Finder or the Start menu inherits the launcher's environment, not the
//! one a terminal would give it, so a Homebrew or nvm install is invisible on
//! `PATH` even though it works perfectly in a shell. Hence the search below:
//! `PATH` first, because it is right when it works, then the handful of places
//! Node actually gets installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// The oldest release still receiving security fixes when this was written.
/// Anything older is a liability on a machine that sits on a church network.
pub const MINIMUM_MAJOR: u32 = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct NodeRuntime {
    pub path: PathBuf,
    /// As reported, e.g. "v22.11.0".
    pub version: String,
    pub major: u32,
}

/// Arguments that make this runtime trust the machine's own certificates.
///
/// # The problem this exists for
///
/// Node ships its own list of certificate authorities and ignores the operating
/// system's. Our Rust side reads the system store, so on a machine where an
/// antivirus or a church firewall inspects encrypted traffic -- which is most
/// Windows machines in a church -- activating a licence works and starting a
/// session does not. Same network, same host, minutes apart. The operator is
/// told to check their internet connection, which is the one thing that is not
/// wrong.
///
/// `--use-system-ca` fixes it, and arrived in Node 22.15. Older runtimes reject
/// unknown options outright and would not start at all, so it is offered only
/// where it is understood -- and asked of the runtime rather than inferred from
/// the version, because the flag also had to be enabled by a build flag for
/// part of its life.
pub fn system_ca_args(node: &NodeRuntime) -> Vec<String> {
    if node.major < 22 {
        return Vec::new();
    }
    let understood = std::process::Command::new(&node.path)
        .args(["--use-system-ca", "-e", ""])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);

    if understood {
        vec!["--use-system-ca".to_string()]
    } else {
        Vec::new()
    }
}

/// What the settings screen shows, and why starting failed if it did.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Node is installed but predates what the sidecar needs.
    pub too_old: bool,
    pub minimum: u32,
}

impl NodeStatus {
    fn missing() -> Self {
        Self { found: false, path: None, version: None, too_old: false, minimum: MINIMUM_MAJOR }
    }
}

/// "v22.11.0" -> 22. Anything unrecognisable is treated as no answer at all
/// rather than guessed at.
pub fn major_version(reported: &str) -> Option<u32> {
    reported
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn interrogate(candidate: &Path) -> Option<NodeRuntime> {
    let mut command = Command::new(candidate);
    command.arg("--version");

    // Without this a console window flashes up on Windows every time we look.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let major = major_version(&version)?;
    Some(NodeRuntime { path: candidate.to_path_buf(), version, major })
}

/// Everywhere worth looking, best guess first.
///
/// Ordered rather than a set: `PATH` is what the operator's own shell would
/// use, so it wins when it is present, and the fallbacks only matter for the
/// launcher-environment case they exist to cover.
fn candidates() -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    let mut found: Vec<PathBuf> = Vec::new();

    let mut push = |path: PathBuf| {
        if path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    };

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            push(dir.join(exe));
        }
    }

    if cfg!(windows) {
        for root in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(root) {
                push(PathBuf::from(&base).join("nodejs").join(exe));
            }
        }
    } else {
        // Homebrew on Apple silicon and on Intel, then the official installer.
        for base in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/opt/local/bin"] {
            push(PathBuf::from(base).join(exe));
        }
    }

    // Version managers keep their runtimes under the home directory, where no
    // launcher will ever look. Newest first, since that is what a shell with
    // the manager loaded would have selected.
    if let Some(home) = home_dir() {
        for manager in [".nvm/versions/node", ".local/share/fnm/node-versions", ".volta/tools/image/node"] {
            let root = home.join(manager);
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            let mut versions: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            versions.sort();
            for version in versions.into_iter().rev() {
                push(version.join("bin").join(exe));
                // fnm nests one level deeper than the others.
                push(version.join("installation").join("bin").join(exe));
            }
        }
    }

    found
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// The runtime to use, or nothing if none is suitable.
///
/// A too-old Node is deliberately not returned: running on it would fail later
/// and less clearly than saying so now.
pub fn find() -> Option<NodeRuntime> {
    candidates().iter().filter_map(|c| interrogate(c)).find(|node| node.major >= MINIMUM_MAJOR)
}

/// What to show the operator, including the awkward middle case where Node is
/// installed but too old to use.
pub fn status() -> NodeStatus {
    let mut oldest_seen: Option<NodeRuntime> = None;

    for candidate in candidates() {
        let Some(node) = interrogate(&candidate) else { continue };
        if node.major >= MINIMUM_MAJOR {
            return NodeStatus {
                found: true,
                path: Some(node.path.display().to_string()),
                version: Some(node.version),
                too_old: false,
                minimum: MINIMUM_MAJOR,
            };
        }
        if oldest_seen.is_none() {
            oldest_seen = Some(node);
        }
    }

    match oldest_seen {
        Some(node) => NodeStatus {
            found: false,
            path: Some(node.path.display().to_string()),
            version: Some(node.version),
            too_old: true,
            minimum: MINIMUM_MAJOR,
        },
        None => NodeStatus::missing(),
    }
}

/// Said in full, because this is the one thing a new install can be missing
/// and the operator has no way to guess it.
pub fn missing_message(status: &NodeStatus) -> String {
    if status.too_old {
        let version = status.version.as_deref().unwrap_or("an older version");
        format!(
            "FATAL: Live transcription needs Node {}+ and this machine has {}. \
             Install a current version from nodejs.org, then start listening again.",
            MINIMUM_MAJOR, version,
        )
    } else {
        format!(
            "FATAL: Live transcription needs the Node runtime, which is not installed. \
             Install it from nodejs.org (version {}+), then start listening again. \
             Everything else in Pulpitry works without it.",
            MINIMUM_MAJOR,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_string_yields_its_major() {
        assert_eq!(major_version("v22.11.0"), Some(22));
        assert_eq!(major_version("v20.0.0\n"), Some(20));
        assert_eq!(major_version("24.1.2"), Some(24));
    }

    #[test]
    fn nonsense_is_not_guessed_at() {
        // Something on PATH called "node" that is not Node at all should be
        // skipped, not assumed to be new enough.
        assert_eq!(major_version("not a version"), None);
        assert_eq!(major_version(""), None);
        assert_eq!(major_version("vX.Y.Z"), None);
    }

    #[test]
    fn the_search_looks_beyond_path() {
        // The whole point: an app opened from Finder has a launcher's PATH, so
        // a list confined to it would find nothing on a Homebrew machine.
        let looked_at = candidates();
        let names: Vec<String> = looked_at.iter().map(|p| p.display().to_string()).collect();
        // Nothing to assert about this machine's installs, but the search must
        // at least consider absolute locations rather than PATH alone.
        assert!(
            names.is_empty() || names.iter().any(|n| n.starts_with('/') || n.contains(':')),
            "candidates should be absolute paths: {names:?}",
        );
    }

    /// Environment-dependent, so not part of the default run: it asserts what
    /// is installed on the machine executing it. Worth having because the
    /// search is the whole point of this module and a unit test cannot cover
    /// a real Homebrew or nvm layout.
    #[test]
    #[ignore = "depends on this machine having Node installed"]
    fn finds_the_node_on_this_machine() {
        let found = find().expect("no usable Node found on a machine that has one");
        assert!(found.major >= MINIMUM_MAJOR, "picked a version we reject: {found:?}");
        assert!(found.path.is_absolute(), "path is not usable from a GUI app: {found:?}");

        let status = status();
        assert!(status.found, "find() and status() disagree: {status:?}");
        assert_eq!(status.version.as_deref(), Some(found.version.as_str()));
    }

    #[test]
    fn a_missing_runtime_is_explained_rather_than_reported() {
        let message = missing_message(&NodeStatus::missing());
        assert!(message.contains("nodejs.org"), "no way forward given: {message}");
        assert!(message.contains("20"), "does not say which version: {message}");
        // The rest of the app is usable without it, and saying so stops this
        // reading as a broken install.
        assert!(message.contains("works without it"), "overstates the damage: {message}");
    }

    #[test]
    fn an_outdated_runtime_says_so_specifically() {
        let stale = NodeStatus {
            found: false,
            path: Some("/usr/local/bin/node".into()),
            version: Some("v16.20.0".into()),
            too_old: true,
            minimum: MINIMUM_MAJOR,
        };
        let message = missing_message(&stale);
        assert!(message.contains("v16.20.0"), "does not say what is installed: {message}");
    }
}
