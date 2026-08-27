//! The connection between the desk and the screen.
//!
//! Newline-delimited JSON over TCP, which is the same shape as the pipe the
//! sidecars already speak and needs no dependency to read or write. What
//! crosses it is about five hundred bytes when a verse changes and nothing at
//! all in between.
//!
//! # What is sent, and why it is not pixels
//!
//! A reference and its lines. The screen renders them with *its own* template,
//! its own fonts and its own layout — which is the point. A church's stream
//! overlay and its projector are deliberately not the same design, and mirroring
//! pixels would force one to wear the other's clothes.
//!
//! It is also the difference between 0.0005 Mbit/s and NDI's hundred, on wifi
//! that is already carrying the audio the recogniser depends on.
//!
//! # A screen that joins late is not behind
//!
//! Whatever is staged is sent the moment a connection is admitted, before
//! anything else. A church starts Castavox after Pulpitry more often than not,
//! and an overlay that stayed blank until the *next* verse would look broken for
//! however long the current passage is being read.
//!
//! # The heartbeat is not for liveness
//!
//! It is how a dead connection is discovered at all. TCP will happily hold a
//! socket open to a machine that has been unplugged, so without something
//! written periodically, the desk keeps a client that is gone and the screen
//! waits for a verse that will never arrive. Writing into the void is what makes
//! it fail.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::pairing::{
    derive_token, new_nonce, prove, Answer, Challenge, Doorkeeper, Pairing, Verdict,
};

/// How often the desk writes something, so a vanished screen is noticed.
pub const HEARTBEAT: Duration = Duration::from_secs(5);

/// How long the screen waits before deciding the desk has gone.
///
/// Three heartbeats. One missed beat is a busy hall network; three is a machine
/// that has gone, and the difference matters because what happens next is a
/// visible reconnection rather than a silent one.
pub const SILENCE_MEANS_GONE: Duration = Duration::from_secs(16);

/// How long a handshake may take before the connection is abandoned.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the accept loop looks up to see whether it has been stopped.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// One verse of a passage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Verse number, or zero for text that is not scripture.
    pub number: i64,
    pub text: String,
}

/// What the desk has staged.
///
/// A closed set rather than free text, and it carries no styling at all. The
/// screen owns how this looks; the desk owns what it says. Anything the desk
/// can stage that a text overlay cannot show — an image, a video — arrives as
/// [`Shown::Nothing`], because a stream overlay silently holding the last verse
/// while the projector shows a photograph would be worse than an honest blank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Shown {
    Scripture {
        /// "John 3:16", as the desk resolved it.
        reference: String,
        translation: String,
        lines: Vec<Line>,
    },
    /// Lyrics, or any authored block. Line breaks are the author's.
    ///
    /// `table` carries a comparison where the desk staged one -- "under the
    /// law" beside "under grace" -- which is often the literal shape of the
    /// argument rather than decoration on it. Flattening it into `lines` was
    /// tried first and is what the desk still does for a screen that cannot
    /// draw one: "Fear: focuses on what might go wrong - Faith: acknowledges
    /// danger" repeated per row, which on a real overlay is a dense block
    /// nobody reads.
    ///
    /// A field on `Words` rather than a variant of its own, deliberately: a
    /// screen built before this existed ignores an unknown field and still
    /// shows the lines, where an unknown *variant* would fail to parse and show
    /// nothing at all. The two ends of this wire are updated separately and by
    /// different churches.
    Words {
        title: String,
        lines: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table: Option<Table>,
    },
    /// Nothing is staged, or what is staged cannot be mirrored as text.
    Nothing,
}

/// A comparison, as rows under headings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// The same comparison as lines, for a screen that draws no tables.
    ///
    /// Each row paired with its heading rather than run together: the columns
    /// are the meaning, and "Fear Faith focuses acknowledges" is not a
    /// comparison, it is four words.
    pub fn as_lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .map(|(at, cell)| match self.header.get(at) {
                        Some(head) if !head.trim().is_empty() => format!("{head}: {cell}"),
                        _ => cell.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ")
            })
            .collect()
    }
}

impl Shown {
    pub fn is_nothing(&self) -> bool {
        matches!(self, Shown::Nothing)
    }
}

/// Everything the desk sends once a connection is admitted.
///
/// `Eq` is gone from this and from [`Event`] because the wind position is a
/// fraction, and a float has no total ordering. Nothing compares these for
/// exact equality outside the tests, which compare literals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    /// What is staged now. Sent on connection and on every change.
    Staged {
        shown: Shown,
        /// How far through a body taller than the screen the desk has wound,
        /// from 0.0 at the top to 1.0 at the end.
        ///
        /// Beside `shown` rather than inside it, because it is not part of what
        /// the desk is saying -- it is which part of it is being said. A screen
        /// that draws the whole thing at once ignores it and loses nothing.
        ///
        /// # Why a fraction and not a frame number
        ///
        /// This was a count of screenfuls, and it did not survive contact with
        /// two screens. The desk broke one lexicon entry into four frames and
        /// the paired overlay broke the same entry into two, because they have
        /// different shapes, different type sizes and different amounts of room
        /// -- so "frame 3" was a place that existed on one of them and not the
        /// other. Winding to the third frame moved the desk and did nothing at
        /// all on the screen.
        ///
        /// A fraction is the one thing both ends can agree on without knowing
        /// anything about each other's layout. It is exact where it matters
        /// most -- 0.0 is the first line on both, 1.0 is the last on both --
        /// and proportional in between, which is the best that is available
        /// when one surface genuinely shows more text than the other.
        ///
        /// Defaulted, so a desk that has been updated can still talk to a
        /// screen that has not, and the other way round. Both then behave as
        /// they did before this existed: the text arrives from the top.
        #[serde(default)]
        progress: f32,
    },
    /// Proof the desk is still there. Carries nothing.
    Beat,
}

/// What a screen reports to whatever is drawing.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Admitted, and by what name the desk knows itself.
    Connected { desk: String },
    /// Paired, with the secret to keep for next time.
    ///
    /// Derived here rather than received: it never crosses the wire, so the
    /// only way the screen can have it is to compute it, and the only moment it
    /// has the ingredients is now. A caller that does not store this will pair
    /// successfully today and be a stranger tomorrow.
    ///
    /// The token and nothing else. This carried a `peer_id` once, which was
    /// *this* machine's — what the desk now knows us by — and a caller
    /// reasonably read it as the desk's and stored it as the address to come
    /// back to. The screen already knows which desk it just connected to; it
    /// was the one thing it did know.
    Paired { token: String },
    Staged(Shown, f32),
    /// The link went. **What was last staged is deliberately not cleared** —
    /// see [`Screen::keep_connected`].
    Dropped { reason: String },
    /// Trying again, in this many seconds. Worth showing somewhere that is not
    /// the stream, so an operator knows why the overlay has stopped changing.
    Reconnecting { seconds: u64 },
    /// Refused for good: the pairing is gone or was never made. Retrying will
    /// not fix it, and the operator has to do something.
    Rejected { reason: String },
}

fn write_line<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()?;
    Ok(())
}

fn read_line<T: for<'a> Deserialize<'a>>(reader: &mut BufReader<TcpStream>) -> Result<T> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return Err(anyhow!("the desk closed the connection")),
        Ok(_) => {}
        // A read timeout, which is the ordinary way a desk that has gone is
        // discovered -- the socket stays open to a machine that is no longer
        // answering. Said in words, because the alternative reached an operator
        // as "Resource temporarily unavailable (os error 35)".
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Err(anyhow!("the desk stopped answering"))
        }
        Err(error) => return Err(anyhow!("the connection failed: {error}")),
    }
    Ok(serde_json::from_str(&line).context("could not read what the other end sent")?)
}

/// The desk: listens, admits, and pushes what is staged.
///
/// Dropping it stops it: the listener closes and every screen is disconnected.
/// That is not tidiness — an operator who turns the mirror off in settings
/// expects the port to close and the other machine to notice, and a `Desk` that
/// merely went out of scope while its threads carried on would leave both true
/// of nothing.
pub struct Desk {
    inner: Arc<Mutex<Inner>>,
    port: u16,
    stopped: Arc<AtomicBool>,
}

struct Inner {
    door: Doorkeeper,
    staged: Shown,
    /// How far through what it staged the desk has wound, so a screen that
    /// joins mid-reading arrives at the same place as the others.
    progress: f32,
    /// One sender per admitted screen. A send that fails is a screen that has
    /// gone, and it is dropped here rather than anywhere else.
    screens: Vec<Sender<Message>>,
    name: String,
}

impl Desk {
    /// Starts listening. Port 0 asks the OS for a free one, which is what the
    /// advertisement then carries.
    pub fn start(name: &str, known: Vec<Pairing>, port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port)).context("could not open the mirror port")?;
        let port = listener.local_addr()?.port();
        // Polled rather than blocked in `accept`, so stopping does not depend on
        // somebody happening to connect. The alternative -- connecting to
        // ourselves to break the block -- works and is a thing to explain
        // forever afterwards.
        listener.set_nonblocking(true).context("could not set the mirror port non-blocking")?;

        let inner = Arc::new(Mutex::new(Inner {
            door: Doorkeeper::new(known),
            staged: Shown::Nothing,
            progress: 0.0,
            screens: Vec::new(),
            name: name.to_string(),
        }));
        let stopped = Arc::new(AtomicBool::new(false));

        {
            let inner = Arc::clone(&inner);
            let stopped = Arc::clone(&stopped);
            std::thread::Builder::new()
                .name("castavox-mirror-accept".into())
                .spawn(move || {
                    while !stopped.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                // Blocking again for this connection: only the
                                // accept loop needs to be interruptible.
                                stream.set_nonblocking(false).ok();
                                let inner = Arc::clone(&inner);
                                // One thread per screen. A church has one or
                                // two, and a handshake that hangs must not hold
                                // up the next one.
                                std::thread::Builder::new()
                                    .name("castavox-mirror-screen".into())
                                    .spawn(move || {
                                        if let Err(error) = serve_one(stream, inner) {
                                            crate::log_line!("[mirror] a screen went: {error:#}");
                                        }
                                    })
                                    .ok();
                            }
                            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(ACCEPT_POLL);
                            }
                            Err(error) => {
                                crate::log_line!("[mirror] could not accept a screen: {error}");
                                std::thread::sleep(ACCEPT_POLL);
                            }
                        }
                    }
                })
                .context("could not start the mirror listener")?;
        }

        Ok(Desk { inner, port, stopped })
    }

    /// Closes the port and disconnects every screen.
    ///
    /// Dropping the senders is what ends each screen's write loop, which closes
    /// its socket, which is how the other machine finds out. Nothing is sent to
    /// announce it: a message saying "goodbye" would be one more thing to get
    /// wrong, and a closed socket is unambiguous.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.inner.lock().unwrap().screens.clear();
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Offers a pairing code for somebody to carry to the other machine.
    pub fn offer_code(&self) -> String {
        self.inner.lock().unwrap().door.offer()
    }

    pub fn withdraw_code(&self) {
        self.inner.lock().unwrap().door.withdraw();
    }

    pub fn pairings(&self) -> Vec<Pairing> {
        self.inner.lock().unwrap().door.pairings()
    }

    pub fn forget(&self, peer_id: &str) -> bool {
        self.inner.lock().unwrap().door.forget(peer_id)
    }

    /// How many screens are connected right now.
    pub fn watching(&self) -> usize {
        self.inner.lock().unwrap().screens.len()
    }

    /// Pushes what is now staged to every screen.
    ///
    /// Also remembered, so a screen connecting afterwards is given it at once
    /// rather than waiting for the next verse.
    pub fn stage(&self, shown: Shown, progress: f32) {
        let mut inner = self.inner.lock().unwrap();
        inner.staged = shown.clone();
        inner.progress = progress;
        let message = Message::Staged { shown, progress };
        inner.screens.retain(|screen| screen.send(message.clone()).is_ok());
    }

    /// Sends a beat, and forgets whatever failed to take it.
    pub fn beat(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.screens.retain(|screen| screen.send(Message::Beat).is_ok());
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_one(stream: TcpStream, inner: Arc<Mutex<Inner>>) -> Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_nodelay(true).ok();

    let mut writing = stream.try_clone()?;
    let mut reading = BufReader::new(stream);

    // A nonce this connection and no other will use.
    let nonce = new_nonce();
    let challenge = {
        let inner = inner.lock().unwrap();
        inner.door.challenge(nonce.clone(), &inner.name, Instant::now())
    };
    write_line(&mut writing, &challenge)?;

    let answer: Answer = read_line(&mut reading)?;
    let verdict = {
        let mut inner = inner.lock().unwrap();
        inner.door.admit(&answer, &nonce, Instant::now())
    };
    write_line(&mut writing, &verdict)?;

    let Verdict::Ready { .. } = verdict else {
        // Refused. The reason went out above; nothing more is owed.
        return Ok(());
    };

    // Admitted. From here the socket is written to and never read from, so the
    // handshake timeout would only get in the way.
    let (tx, rx): (Sender<Message>, Receiver<Message>) = channel();
    {
        let mut inner = inner.lock().unwrap();
        // Whatever is staged, before anything else: a screen that joined
        // mid-passage should show the passage, not wait for the next one.
        let staged = inner.staged.clone();
        let progress = inner.progress;
        inner.screens.push(tx);
        drop(inner);
        write_line(&mut writing, &Message::Staged { shown: staged, progress })?;
    }

    // Ends when the desk drops the sender, or when a write fails because the
    // screen has gone.
    while let Ok(message) = rx.recv() {
        write_line(&mut writing, &message)?;
    }
    Ok(())
}

/// How long to wait before the first retry, and how far the wait may grow.
///
/// The ceiling is low on purpose. This is a live service: a church wifi blip
/// lasting seconds should cost seconds, and a backoff that has climbed to a
/// minute means the verse the preacher is on now arrives after he has finished
/// with it. The cost of trying often is a TCP connection to a machine in the
/// same room.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(8);

/// The screen: connects, proves itself, and reports what arrives.
///
/// # What it does not do when the link drops
///
/// It does not clear. A blank overlay behind a preacher is worse than one a few
/// seconds behind — clearing is something an operator does on purpose, and a
/// network blip is not that. The last verse stays on screen and the caller is
/// told the link went, so it can say so somewhere that is not the stream.
///
/// This is a property of the *protocol*, not only of the drawing code: nothing
/// in a dropped connection produces a [`Shown::Nothing`]. A blank arrives only
/// when the desk deliberately staged one, so a screen cannot be cleared by a
/// bad network — it can only be cleared by a person.
pub struct Screen;

impl Screen {
    /// Connects and runs until the link fails, reporting as it goes.
    ///
    /// `secret` is the pairing code the first time, and the stored token every
    /// time after. `on_paired` is called with the derived token exactly once, on
    /// a successful pairing, so the caller can store it.
    pub fn run(
        address: impl ToSocketAddrs,
        id: &str,
        name: &str,
        secret: &Secret,
        report: impl Fn(Event),
    ) -> Result<()> {
        let address = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("no address to connect to"))?;

        let stream = TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT)
            .context("could not reach the desk")?;
        stream.set_nodelay(true).ok();
        // Silence beyond this is a desk that has gone. It has to be longer than
        // the heartbeat or an idle service would look like a failure.
        stream.set_read_timeout(Some(SILENCE_MEANS_GONE))?;

        let mut writing = stream.try_clone()?;
        let mut reading = BufReader::new(stream);

        let challenge: Challenge = read_line(&mut reading)?;

        // Computed before the answer goes out, from the same three ingredients
        // the desk will use. Reported only if the desk agrees.
        let mut derived = String::new();

        let answer = match secret {
            Secret::Code(code) => {
                if !challenge.pairing_open {
                    report(Event::Rejected {
                        reason: "That desk is not offering a code. Press Pair on it first.".into(),
                    });
                    return Ok(());
                }
                derived = derive_token(code, id, &challenge.nonce);
                Answer::Pair {
                    id: id.to_string(),
                    name: name.to_string(),
                    proof: prove(code, &challenge.nonce),
                }
            }
            Secret::Token(token) => Answer::Hello {
                id: id.to_string(),
                proof: prove(token, &challenge.nonce),
            },
        };
        write_line(&mut writing, &answer)?;

        match read_line::<Verdict>(&mut reading)? {
            // The desk's name from the challenge, not the name the verdict
            // echoes back -- that one is us, and reporting it here told the
            // operator they had connected to their own machine.
            Verdict::Ready { .. } => {
                // Before Connected, so a caller storing it on this event has it
                // safe before anything else can go wrong.
                if !derived.is_empty() {
                    report(Event::Paired { token: std::mem::take(&mut derived) });
                }
                report(Event::Connected { desk: challenge.desk.clone() });
            }
            // Not retried. A refusal is a decision, not a fault, and reconnecting
            // into it would spin against a desk that has already said no.
            Verdict::Refused { reason } => {
                report(Event::Rejected { reason });
                return Ok(());
            }
        }

        loop {
            match read_line::<Message>(&mut reading) {
                Ok(Message::Staged { shown, progress }) => report(Event::Staged(shown, progress)),
                Ok(Message::Beat) => {}
                Err(error) => {
                    report(Event::Dropped { reason: error.to_string() });
                    return Ok(());
                }
            }
        }
    }
}

impl Screen {
    /// Stays connected for as long as `keep_going` says to, reconnecting when
    /// the link fails.
    ///
    /// `find_desk` is asked for an address on every attempt rather than once, so
    /// a desk that came back on a different address — a new DHCP lease after the
    /// router restarted, which is a thing that happens to a church hall — is
    /// found again without anybody touching either machine.
    ///
    /// Returns when `keep_going` goes false, or when the desk refuses. A refusal
    /// is a decision rather than a fault: retrying into it would spin forever
    /// against a desk that has already said no, and the operator needs to know
    /// rather than wait.
    pub fn keep_connected(
        find_desk: impl Fn() -> Option<std::net::SocketAddr>,
        id: &str,
        name: &str,
        token: &str,
        keep_going: impl Fn() -> bool,
        report: impl Fn(Event),
    ) {
        let mut wait = RETRY_FIRST;
        // Set by the wrapper below when the desk refuses. `run` reports a
        // refusal and returns Ok -- it is not an error, it is an answer -- so
        // without this the loop would treat it as an ordinary disconnection and
        // retry into a "no" for the rest of the service.
        let refused = std::cell::Cell::new(false);

        while keep_going() {
            let attempt = match find_desk() {
                Some(address) => Screen::run(
                    address,
                    id,
                    name,
                    &Secret::Token(token.to_string()),
                    |event| {
                        if matches!(event, Event::Rejected { .. }) {
                            refused.set(true);
                        }
                        report(event);
                    },
                ),
                None => Err(anyhow!("no desk could be found on this network")),
            };

            if refused.get() {
                return;
            }

            // A successful `run` returns when the link ended, which is ordinary.
            // An error is a connection that never opened.
            if let Err(error) = attempt {
                report(Event::Dropped { reason: error.to_string() });
            }

            if !keep_going() {
                return;
            }

            report(Event::Reconnecting { seconds: wait.as_secs() });
            // Slept in slices so stopping does not have to wait out a backoff.
            let until = Instant::now() + wait;
            while Instant::now() < until {
                if !keep_going() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100).min(RETRY_FIRST));
            }

            wait = (wait * 2).min(RETRY_LONGEST);
        }
    }
}

/// What the screen proves itself with.
pub enum Secret {
    /// First time: the code somebody carried across the room.
    Code(String),
    /// Every time after.
    Token(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::pairing::derive_token;
    use std::sync::mpsc::channel as std_channel;

    /// The next event that is not the pairing handover.
    ///
    /// Three tests pair with a code and then want the connection, and `Paired`
    /// arrives first by design -- the token has to be safe before anything else
    /// can go wrong.
    fn next_after_pairing(
        rx: &std::sync::mpsc::Receiver<Event>,
    ) -> Event {
        loop {
            let event = rx.recv_timeout(Duration::from_secs(3)).expect("should have answered");
            if !matches!(event, Event::Paired { .. }) {
                return event;
            }
        }
    }

    fn psalm() -> Shown {
        Shown::Scripture {
            reference: "John 3:16".into(),
            translation: "KJV".into(),
            lines: vec![Line { number: 16, text: "For God so loved the world".into() }],
        }
    }

    /// Pairs a screen against a desk and returns the token both ends derived.
    fn pair(desk: &Desk) -> (String, Vec<Event>) {
        let code = desk.offer_code();
        let (tx, rx) = std_channel();
        let port = desk.port();

        let handle = std::thread::spawn(move || {
            Screen::run(
                ("127.0.0.1", port),
                "screen-1",
                "Stream machine",
                &Secret::Code(code),
                move |event| {
                    let _ = tx.send(event);
                },
            )
        });

        // Enough to admit and deliver the first push.
        let mut seen = Vec::new();
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(3)) {
            let last = matches!(event, Event::Staged(..));
            seen.push(event);
            if last {
                break;
            }
        }

        // The token is not on the wire, so it is derived the same way the desk
        // did -- which is the property being relied on.
        let token = desk.pairings().first().map(|p| p.token.clone()).unwrap_or_default();
        drop(handle);
        (token, seen)
    }

    #[test]
    fn a_screen_that_predates_tables_still_reads_the_words() {
        // The two ends of this wire are updated separately and by different
        // churches, so a desk that has learnt about tables will be talking to
        // screens that have not. An unknown field is ignored; an unknown
        // variant would have shown nothing at all.
        let sent = serde_json::to_string(&Shown::Words {
            title: "Fear And Faith".into(),
            lines: vec!["Fear: focuses on what might go wrong".into()],
            table: Some(Table {
                header: vec!["Fear".into(), "Faith".into()],
                rows: vec![vec!["Focuses on danger".into(), "Acts anyway".into()]],
            }),
        })
        .unwrap();

        #[derive(serde::Deserialize)]
        #[serde(tag = "kind", rename_all = "camelCase")]
        enum Older {
            Scripture {},
            Words { title: String, lines: Vec<String> },
            Nothing,
        }

        match serde_json::from_str::<Older>(&sent).expect("an older screen could not read it") {
            Older::Words { title, lines } => {
                assert_eq!(title, "Fear And Faith");
                assert_eq!(lines.len(), 1, "the lines it can draw are still there");
            }
            _ => panic!("read as the wrong thing"),
        }
    }

    #[test]
    fn a_comparison_as_lines_keeps_its_headings() {
        let table = Table {
            header: vec!["Activity".into(), "Progress".into()],
            rows: vec![vec!["Keeps busy".into(), "Arrives".into()]],
        };
        assert_eq!(table.as_lines(), vec!["Activity: Keeps busy \u{b7} Progress: Arrives"]);
    }

    #[test]
    fn a_screen_pairs_and_is_told_what_is_staged() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        desk.stage(psalm(), 0.0);

        let (token, seen) = pair(&desk);

        assert!(!token.is_empty(), "the desk should have remembered the pairing");
        // The token comes first, so it is safe before anything else can fail.
        assert!(matches!(seen.first(), Some(Event::Paired { .. })));
        // And the *desk's* name, not this screen's. Reporting the verdict's
        // name here told the operator they had connected to their own machine.
        assert!(seen.contains(&Event::Connected { desk: "The Desk".into() }));
        // Staged before this screen existed, and given to it on arrival: a
        // church starts Castavox after Pulpitry more often than not.
        assert_eq!(seen.last(), Some(&Event::Staged(psalm(), 0.0)));
    }

    #[test]
    fn what_is_already_staged_arrives_before_anything_changes() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        desk.stage(psalm(), 0.0);
        let (_, seen) = pair(&desk);

        let staged: Vec<&Event> = seen.iter().filter(|e| matches!(e, Event::Staged(..))).collect();
        assert_eq!(staged.len(), 1, "exactly one push, without the desk staging again");
    }

    #[test]
    fn a_remembered_screen_needs_no_code() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let (token, _) = pair(&desk);
        desk.withdraw_code();

        // No code is being offered now, which is the ordinary state of a desk
        // in the middle of a service.
        let (tx, rx) = std_channel();
        let port = desk.port();
        std::thread::spawn(move || {
            Screen::run(("127.0.0.1", port), "screen-1", "Stream machine", &Secret::Token(token), move |e| {
                let _ = tx.send(e);
            })
        });

        let first = next_after_pairing(&rx);
        assert!(matches!(first, Event::Connected { .. }), "got {first:?}");
    }

    #[test]
    fn a_screen_that_was_never_paired_is_told_so_and_does_not_retry() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let (tx, rx) = std_channel();
        let port = desk.port();

        std::thread::spawn(move || {
            Screen::run(
                ("127.0.0.1", port),
                "stranger",
                "Somebody's laptop",
                &Secret::Token("made-up".into()),
                move |e| {
                    let _ = tx.send(e);
                },
            )
        });

        let first = rx.recv_timeout(Duration::from_secs(3)).expect("should have answered");
        assert!(matches!(first, Event::Rejected { .. }), "got {first:?}");
        assert_eq!(desk.watching(), 0);
    }

    #[test]
    fn staging_reaches_a_connected_screen() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let code = desk.offer_code();
        let (tx, rx) = std_channel();
        let port = desk.port();

        std::thread::spawn(move || {
            Screen::run(("127.0.0.1", port), "screen-1", "Stream", &Secret::Code(code), move |e| {
                let _ = tx.send(e);
            })
        });

        // Connected, then the empty state it joined into.
        assert!(matches!(next_after_pairing(&rx), Event::Connected { .. }));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(Shown::Nothing, 0.0));

        desk.stage(psalm(), 0.0);
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(psalm(), 0.0));

        // And a clear is a message like any other, so an operator clearing the
        // projector clears the overlay too.
        desk.stage(Shown::Nothing, 0.0);
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(Shown::Nothing, 0.0));
    }

    #[test]
    fn the_screen_is_given_the_token_it_will_need_tomorrow() {
        // It never crosses the wire, so the only way the screen can have it is
        // to derive it, and the only moment it has the ingredients is during
        // the handshake. A caller that is not handed it here pairs today and is
        // a stranger next Sunday.
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let code = desk.offer_code();
        let port = desk.port();
        let (tx, rx) = std_channel();

        std::thread::spawn(move || {
            Screen::run(("127.0.0.1", port), "screen-1", "Stream", &Secret::Code(code), move |e| {
                let _ = tx.send(e);
            })
        });

        let first = rx.recv_timeout(Duration::from_secs(3)).expect("should have paired");
        let Event::Paired { token } = first else {
            panic!("expected the token first, got {first:?}");
        };

        // And it is the same one the desk kept, or the next connection fails.
        assert_eq!(token, desk.pairings()[0].token);
        assert!(!token.is_empty());
    }

    #[test]
    fn a_dropped_link_never_produces_a_blank() {
        /*
         * The property the whole feature rests on, asserted against the
         * protocol rather than against the drawing code.
         *
         * A screen going blank behind a preacher is the worst thing this can
         * do -- worse than being a few seconds behind, worse than not
         * connecting at all, because the operator sees an empty overlay and has
         * no idea whether it is coming back. Clearing is something a person
         * does on purpose.
         *
         * So: kill the desk mid-service and prove that what arrives is a
         * Dropped and nothing else. No Staged(Nothing), from anywhere.
         */
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let port = desk.port();
        let code = desk.offer_code();
        let (tx, rx) = std_channel();

        std::thread::spawn(move || {
            Screen::run(("127.0.0.1", port), "screen-1", "Stream", &Secret::Code(code), move |e| {
                let _ = tx.send(e);
            })
        });

        assert!(matches!(next_after_pairing(&rx), Event::Connected { .. }));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(Shown::Nothing, 0.0));
        desk.stage(psalm(), 0.0);
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(psalm(), 0.0));

        // The desk goes: machine asleep, cable out, wifi gone.
        drop(desk);

        let after = rx.recv_timeout(Duration::from_secs(5)).expect("should have noticed");
        assert!(matches!(after, Event::Dropped { .. }), "got {after:?}");

        // And nothing follows it that would wipe the screen.
        while let Ok(event) = rx.recv_timeout(Duration::from_millis(300)) {
            assert_ne!(
                event,
                Event::Staged(Shown::Nothing, 0.0),
                "a dropped link must never blank the overlay",
            );
        }
    }

    #[test]
    fn a_refusal_stops_the_loop_rather_than_spinning_against_a_no() {
        // A refusal is an answer, not a fault. `run` reports it and returns Ok,
        // so a loop that only watched for errors would retry into "no" for the
        // rest of the service and never tell anybody why.
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        let address: std::net::SocketAddr = ([127, 0, 0, 1], desk.port()).into();
        let (tx, rx) = std_channel();

        let ran = std::thread::spawn(move || {
            Screen::keep_connected(
                || Some(address),
                "stranger",
                "Somebody's laptop",
                "a-token-nobody-issued",
                || true, // never asked to stop: only a refusal can end this
                move |event| {
                    let _ = tx.send(event);
                },
            );
        });

        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Rejected { .. }
        ));
        ran.join().expect("the loop should have ended on its own");
    }

    #[test]
    fn it_keeps_trying_while_the_desk_is_away() {
        // A church hall router restarts. Nobody is at either machine.
        let (tx, rx) = std_channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let stopper = Arc::clone(&stop);
        let ran = std::thread::spawn(move || {
            Screen::keep_connected(
                // Nothing there yet, which is what a desk not started looks like.
                || None,
                "screen-1",
                "Stream",
                "a-token",
                move || !stopper.load(std::sync::atomic::Ordering::Relaxed),
                move |event| {
                    let _ = tx.send(event);
                },
            );
        });

        // It reports the failure and says when it will try again, rather than
        // going quiet -- the operator needs somewhere to see why the overlay
        // has stopped changing.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Dropped { .. }));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Reconnecting { .. }
        ));

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        ran.join().expect("should stop when asked");
    }

    #[test]
    fn the_token_is_derived_identically_at_both_ends() {
        // The property the whole handshake rests on: it is never transmitted,
        // so it can only be right if both ends computed it from the same
        // ingredients.
        let nonce = new_nonce();
        assert_eq!(
            derive_token("ABC234", "screen-1", &nonce),
            derive_token("ABC234", "screen-1", &nonce),
        );
    }
}
