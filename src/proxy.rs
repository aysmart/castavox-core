//! Finding the proxy a church's network requires.
//!
//! # Why this is ours to find rather than Node's
//!
//! Browsers on Windows read the proxy configured in Windows Settings. Node does
//! not: neither `fetch` nor `ws` consults it, and neither reads the registry.
//! So on a filtered church or school network every browser works, a licence
//! activates, and the speech bridge alone cannot reach anything — which looks
//! from the desk like the software being broken on that machine only.
//!
//! Rust is where the bridge is launched, so Rust is where the answer can be
//! found and handed over. The bridge honours `HTTPS_PROXY` when it is set; this
//! makes sure it is set on the machines that need it, without asking a
//! volunteer to know what a proxy is.
//!
//! An operator who has set the variable themselves is believed and left alone:
//! an explicit setting beats anything discovered.

/// The proxy to use for outbound HTTPS, if there is one.
///
/// Returns a URL suitable for `HTTPS_PROXY`. `None` means direct, which is the
/// answer on almost every home and church connection.
pub fn https_proxy() -> Option<String> {
    for name in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    windows_proxy()
}

/// What Windows itself is configured to use.
///
/// Read with `reg query` rather than a registry crate: it is two values on one
/// key, read once at the start of a session, and a dependency for that would be
/// carried by every platform to serve one.
#[cfg(windows)]
fn windows_proxy() -> Option<String> {
    const KEY: &str =
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    let read = |value: &str| -> Option<String> {
        let out = std::process::Command::new("reg")
            .args(["query", KEY, "/v", value])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        // "    ProxyServer    REG_SZ    proxy.church.local:8080"
        let line = text.lines().find(|line| line.contains(value))?;
        Some(line.split_whitespace().last()?.to_string())
    };

    // Configured but switched off is the common case on a machine that was once
    // on a filtered network; honouring it anyway would break a working install.
    if read("ProxyEnable").as_deref() != Some("0x1") {
        return None;
    }

    let server = read("ProxyServer")?;
    if server.trim().is_empty() {
        return None;
    }

    /*
     * Windows allows a per-protocol list -- "http=a:80;https=b:443" -- as well
     * as one address for everything. The https entry wins where there is one,
     * because that is what the bridge speaks.
     */
    let chosen = server
        .split(';')
        .find_map(|part| part.trim().strip_prefix("https=").map(str::to_string))
        .or_else(|| {
            server
                .split(';')
                .find_map(|part| part.trim().strip_prefix("http=").map(str::to_string))
        })
        .unwrap_or_else(|| server.trim().to_string());

    if chosen.is_empty() {
        return None;
    }
    // Windows stores host:port; a URL is what an HTTPS_PROXY consumer expects.
    Some(if chosen.contains("://") { chosen } else { format!("http://{chosen}") })
}

#[cfg(not(windows))]
fn windows_proxy() -> Option<String> {
    // Every other platform puts it in the environment, which is read above.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because these share the process environment.
    ///
    /// Split in two they raced: Rust runs tests on parallel threads, and the
    /// one clearing the variables cleared them out from under the one that had
    /// just set them. A failure that appears only sometimes is worse than the
    /// coverage of a second test name.
    #[test]
    #[cfg(not(windows))]
    fn the_environment_decides_and_blank_means_none() {
        std::env::set_var("HTTPS_PROXY", "http://chosen.example:3128");
        assert_eq!(
            https_proxy().as_deref(),
            Some("http://chosen.example:3128"),
            "an explicit setting is believed"
        );

        // Set to nothing means "no proxy", not "a proxy called nothing" --
        // passing the blank along would point the bridge at an empty address
        // and fail every request with something unreadable.
        std::env::set_var("HTTPS_PROXY", "   ");
        assert_eq!(https_proxy(), None, "blank is not a proxy");

        std::env::remove_var("HTTPS_PROXY");
        assert_eq!(https_proxy(), None, "unset is not a proxy");
    }
}
