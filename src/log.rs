//! Where this crate's diagnostics go.
//!
//! The two hosts want different things. Pulpitry writes to a file beside its
//! data, because a release build on Windows is a GUI binary with no console and
//! anything written to stderr is simply lost. Castavox's sidecar writes to
//! stderr, which its host process captures alongside OBS's own log.
//!
//! Rather than pick one, the crate writes to whatever the host installs. Absent
//! a sink it falls back to stderr, so a module still says something useful in a
//! test or a `cargo run`.

use std::sync::OnceLock;

/// Where lines go once a host has said.
type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Directs this crate's diagnostics somewhere.
///
/// Called once, early, by the host. Later calls are ignored rather than
/// racing: two sinks would mean two copies of every line, and the second
/// caller is nearly always a test that ran in the wrong order.
pub fn to<F>(sink: F)
where
    F: Fn(&str) + Send + Sync + 'static,
{
    let _ = SINK.set(Box::new(sink));
}

/// Writes one line.
pub fn line(message: &str) {
    match SINK.get() {
        Some(sink) => sink(message),
        // Useful in a test or a bare `cargo run`, and harmless in a GUI build
        // where nothing is listening.
        None => eprintln!("{message}"),
    }
}

/// Writes a line. Use exactly like `eprintln!`.
#[macro_export]
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_line_reaches_the_installed_sink() {
        let caught: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&caught);
        to(move |message| seen.lock().unwrap().push(message.to_string()));

        line("the father ran");
        assert_eq!(caught.lock().unwrap().as_slice(), ["the father ran"]);

        // A second host does not get to double every line.
        to(|_| panic!("the second sink should never be called"));
        line("and again");
        assert_eq!(caught.lock().unwrap().len(), 2);
    }
}
