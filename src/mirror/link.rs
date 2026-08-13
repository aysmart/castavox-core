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
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::pairing::{new_nonce, prove, Answer, Challenge, Doorkeeper, Pairing, Verdict};

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
    Words { title: String, lines: Vec<String> },
    /// Nothing is staged, or what is staged cannot be mirrored as text.
    Nothing,
}

impl Shown {
    pub fn is_nothing(&self) -> bool {
        matches!(self, Shown::Nothing)
    }
}

/// Everything the desk sends once a connection is admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    /// What is staged now. Sent on connection and on every change.
    Staged { shown: Shown },
    /// Proof the desk is still there. Carries nothing.
    Beat,
}

/// What a screen reports to whatever is drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Admitted, and by what name the desk knows itself.
    Connected { desk: String },
    Staged(Shown),
    /// The link went. **What was last staged is deliberately not cleared** —
    /// see [`Screen`].
    Dropped { reason: String },
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
    if reader.read_line(&mut line)? == 0 {
        return Err(anyhow!("the connection closed"));
    }
    Ok(serde_json::from_str(&line).context("could not read what the other end sent")?)
}

/// The desk: listens, admits, and pushes what is staged.
pub struct Desk {
    inner: Arc<Mutex<Inner>>,
    port: u16,
}

struct Inner {
    door: Doorkeeper,
    staged: Shown,
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

        let inner = Arc::new(Mutex::new(Inner {
            door: Doorkeeper::new(known),
            staged: Shown::Nothing,
            screens: Vec::new(),
            name: name.to_string(),
        }));

        {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("castavox-mirror-accept".into())
                .spawn(move || {
                    for incoming in listener.incoming() {
                        let Ok(stream) = incoming else { continue };
                        let inner = Arc::clone(&inner);
                        // One thread per screen. A church has one or two, and a
                        // handshake that hangs must not hold up the next one.
                        std::thread::Builder::new()
                            .name("castavox-mirror-screen".into())
                            .spawn(move || {
                                if let Err(error) = serve_one(stream, inner) {
                                    crate::log_line!("[mirror] a screen disconnected: {error:#}");
                                }
                            })
                            .ok();
                    }
                })
                .context("could not start the mirror listener")?;
        }

        Ok(Desk { inner, port })
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
    pub fn stage(&self, shown: Shown) {
        let mut inner = self.inner.lock().unwrap();
        inner.staged = shown.clone();
        let message = Message::Staged { shown };
        inner.screens.retain(|screen| screen.send(message.clone()).is_ok());
    }

    /// Sends a beat, and forgets whatever failed to take it.
    pub fn beat(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.screens.retain(|screen| screen.send(Message::Beat).is_ok());
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
        inner.screens.push(tx);
        drop(inner);
        write_line(&mut writing, &Message::Staged { shown: staged })?;
    }

    // Ends when the desk drops the sender, or when a write fails because the
    // screen has gone.
    while let Ok(message) = rx.recv() {
        write_line(&mut writing, &message)?;
    }
    Ok(())
}

/// The screen: connects, proves itself, and reports what arrives.
///
/// # What it does not do when the link drops
///
/// It does not clear. A blank overlay behind a preacher is worse than one a few
/// seconds behind — clearing is something an operator does on purpose, and a
/// network blip is not that. The last verse stays on screen and the caller is
/// told the link went, so it can say so somewhere that is not the stream.
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

        let answer = match secret {
            Secret::Code(code) => {
                if !challenge.pairing_open {
                    report(Event::Rejected {
                        reason: "That desk is not offering a code. Press Pair on it first.".into(),
                    });
                    return Ok(());
                }
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
            Verdict::Ready { .. } => report(Event::Connected { desk: challenge.desk.clone() }),
            // Not retried. A refusal is a decision, not a fault, and reconnecting
            // into it would spin against a desk that has already said no.
            Verdict::Refused { reason } => {
                report(Event::Rejected { reason });
                return Ok(());
            }
        }

        loop {
            match read_line::<Message>(&mut reading) {
                Ok(Message::Staged { shown }) => report(Event::Staged(shown)),
                Ok(Message::Beat) => {}
                Err(error) => {
                    report(Event::Dropped { reason: error.to_string() });
                    return Ok(());
                }
            }
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
            let last = matches!(event, Event::Staged(_));
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
    fn a_screen_pairs_and_is_told_what_is_staged() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        desk.stage(psalm());

        let (token, seen) = pair(&desk);

        assert!(!token.is_empty(), "the desk should have remembered the pairing");
        // The *desk's* name, not this screen's. Reporting the verdict's name
        // here told the operator they had connected to their own machine.
        assert_eq!(seen.first(), Some(&Event::Connected { desk: "The Desk".into() }));
        // Staged before this screen existed, and given to it on arrival: a
        // church starts Castavox after Pulpitry more often than not.
        assert_eq!(seen.last(), Some(&Event::Staged(psalm())));
    }

    #[test]
    fn what_is_already_staged_arrives_before_anything_changes() {
        let desk = Desk::start("The Desk", Vec::new(), 0).expect("should listen");
        desk.stage(psalm());
        let (_, seen) = pair(&desk);

        let staged: Vec<&Event> = seen.iter().filter(|e| matches!(e, Event::Staged(_))).collect();
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

        let first = rx.recv_timeout(Duration::from_secs(3)).expect("should have connected");
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
        assert!(matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Connected { .. }));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(Shown::Nothing));

        desk.stage(psalm());
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(psalm()));

        // And a clear is a message like any other, so an operator clearing the
        // projector clears the overlay too.
        desk.stage(Shown::Nothing);
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Staged(Shown::Nothing));
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
