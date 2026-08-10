//! What Castavox and Pulpitry both need.
//!
//! The two products present differently -- one is a broadcast studio built on
//! OBS, the other a presentation app for a projector -- but underneath they do
//! several identical things: capture a microphone, transcribe on the machine,
//! keep a transcript, turn a summary into a document a church can send on, and
//! match paraphrased scripture against an embedding index.
//!
//! Those parts lived as copies in both repositories, and copies drift. One fix
//! -- installing a TLS provider before the first HTTPS request -- landed in one
//! product and not the other, where it left model downloads working only by
//! coincidence for months. This crate exists so that cannot happen again.
//!
//! # What belongs here
//!
//! Anything that needs no opinion about the host. Nothing here knows about
//! Tauri, Qt, OBS, windows, or how settings are stored, because those are
//! exactly the places the two products genuinely differ. Modules that would
//! need such an opinion take it as a parameter -- see [`log`] for the pattern.

pub mod audio;
pub mod embed;
pub mod exports;
pub mod hosted;
pub mod log;
pub mod node;
pub mod tls;
pub mod transcripts;
pub mod whisper;
