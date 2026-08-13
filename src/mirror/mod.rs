//! One church, two machines, one transcription.
//!
//! Pulpitry at the operator's desk and Castavox on the streaming machine are
//! usually in the same room, listening to the same preacher, separately. That
//! is two microphones, two transcriptions, and — on a subscription — two
//! streams of audio billed for one sermon.
//!
//! The mirror makes it one. Pulpitry listens and stages; Castavox shows what
//! was staged.
//!
//! - [`discovery`] finds the desk on the network, and nothing more.
//! - [`pairing`] decides whether it should be listened to, which is the harder
//!   half: church wifi is one password printed on a wall, so being reachable
//!   proves nothing about who you are.
//!
//! The split is deliberate. An advertisement is public by nature — anything on
//! the network can read it — so nothing in discovery is secret and nothing in
//! it is taken on faith. Every question of trust lives in the other module.

pub mod discovery;
pub mod link;
pub mod pairing;

pub use discovery::{advertise, browse, find, Advertisement, Discovery, Peer, SERVICE_TYPE};
pub use link::{Desk, Event, Line, Screen, Secret, Shown};
pub use pairing::{Answer, Challenge, Doorkeeper, Pairing, Verdict};
