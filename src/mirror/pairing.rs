//! Deciding whether the machine on the other end is allowed to drive the screen.
//!
//! [`super::discovery`] answers "what is out there". This answers "and should it
//! be listened to", which is the harder half.
//!
//! # What this is defending against
//!
//! A socket on the church wifi that anything can drive is a way for anyone in
//! the building to put words on the stream. Not a hypothetical: church wifi is
//! usually one password, printed on a wall, shared with the congregation. The
//! guest on row six is inside the network by design.
//!
//! So a peer is not trusted for being reachable. Pulpitry shows a short code,
//! somebody carries it to the other machine, and that act — a person, crossing
//! a room, who has access to both — is what the trust rests on.
//!
//! # Nothing secret ever crosses the wire
//!
//! Neither the code nor the token is ever transmitted, at pairing or after it.
//!
//! 1. The desk sends a random `nonce`.
//! 2. The screen answers with `HMAC(code, nonce)` — proof it knows the code,
//!    which reveals nothing about the code itself.
//! 3. Both ends independently derive the same long-lived token from the code,
//!    the nonce and the peer's id. It is stored at both ends and never sent.
//! 4. Every later connection proves the token the same way, against a fresh
//!    nonce.
//!
//! Deriving the token rather than issuing it is what makes this worth doing.
//! Handing one back over a plain socket would put the long-lived secret on a
//! network whose password is on a wall, once, in the clear — and once is enough
//! for anybody who happened to be capturing.
//!
//! A fresh nonce per connection is what stops a captured proof being replayed.
//!
//! # What it is not
//!
//! Not encryption. What follows the handshake — a verse reference and its
//! text — travels in the clear, and is content a church is about to project on
//! a wall in front of everybody. Encrypting it would mean certificates, which
//! means a trust decision an operator cannot make and a class of failure
//! ("certificate expired") in the middle of a service. The secret is protected;
//! the psalm is not, and does not need to be.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Characters a code is drawn from.
///
/// No `0`/`O`, no `1`/`I`/`L`. Somebody is reading this off one screen and
/// typing it into another, in a hall, possibly in the dark, and a code that
/// invites a transcription error is a code that gets typed wrong and blamed on
/// the software.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// How many characters. Six of this alphabet is about 30 bits — nine hundred
/// million codes — which against the attempt limit below is not guessable.
const CODE_LENGTH: usize = 6;

/// How long a code is offered before it goes stale.
///
/// Long enough to walk to another machine and type it. Short enough that a code
/// left on a screen after a service is not still valid on Sunday week.
pub const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// Wrong answers before pairing closes.
///
/// Six characters and five attempts is not a lock somebody picks. The count is
/// per code, and offering a new one clears it, so an operator who mistyped is
/// never stuck — they press the button again.
const MAX_ATTEMPTS: u32 = 5;

/// Domain separation, so a proof of one thing can never be a proof of another.
const PROOF_CONTEXT: &[u8] = b"castavox-mirror-proof-v1";
const TOKEN_CONTEXT: &[u8] = b"castavox-mirror-token-v1";

type HmacSha256 = Hmac<Sha256>;

/// What one paired screen is remembered as. Persisted by each product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    /// The peer's installation id — stable across a rename or a new address.
    pub peer_id: String,
    /// What to call it on screen.
    pub name: String,
    /// The derived secret. Never transmitted by either end.
    pub token: String,
}

/// What the desk says first, on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub nonce: String,
    /// Whether a code is being offered right now, so the screen can say
    /// "ask them to press Pair" rather than failing with nothing to act on.
    pub pairing_open: bool,
    /// What the desk calls itself, so the screen can name what it is about to
    /// trust *before* it commits to it. Not a disclosure: the same name is in
    /// the mDNS advertisement, which anything on the network can already read.
    pub desk: String,
}

/// What the screen answers with.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Answer {
    /// First time: proof of the code a person carried across the room.
    Pair { id: String, name: String, proof: String },
    /// Every time after: proof of the token derived when they paired.
    Hello { id: String, proof: String },
}

/// How the desk replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Verdict {
    /// Come in.
    ///
    /// `you` is what the desk now knows the *screen* as, not what the desk
    /// calls itself — the desk's own name is in the challenge. Named this way
    /// because the previous spelling was `name`, and the screen duly reported
    /// its own name back to its operator as the name of the desk it had
    /// connected to.
    Ready { you: String },
    /// Not coming in, and why — in words the operator at the *screen* can act
    /// on, since that is who will read it.
    Refused { reason: String },
}

/// A random code, for a person to read aloud or carry.
pub fn new_code() -> String {
    let mut raw = [0u8; CODE_LENGTH];
    // A failure here would mean the OS has no randomness, which is not a
    // condition to paper over with a predictable code.
    getrandom::fill(&mut raw).expect("the OS must provide randomness");
    raw.iter().map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char).collect()
}

/// A random nonce, fresh per connection, so a captured proof cannot be reused.
pub fn new_nonce() -> String {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).expect("the OS must provide randomness");
    hex(&raw)
}

/// Tidies a code a human typed: case, and whatever they put between the groups.
///
/// There is deliberately no mapping of confusable characters here, and an
/// earlier version of this had one — `O` to `0`, `I` and `L` to `1` — which was
/// wasted work dressed up as care. The alphabet contains *neither* member of
/// each of those pairs, so there is nothing to map to: a code never has an `O`
/// or a `0` in it. Somebody who types one has misread some other character
/// entirely, and no substitution can guess which.
///
/// The confusion is handled where it can be, which is in [`ALPHABET`] by never
/// issuing those characters in the first place.
pub fn normalise(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// Proof that the answerer knows `secret`, without disclosing it.
pub fn prove(secret: &str, nonce: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(PROOF_CONTEXT);
    mac.update(nonce.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// The long-lived secret, derived identically at both ends and never sent.
pub fn derive_token(code: &str, peer_id: &str, nonce: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(code.as_bytes()).expect("HMAC takes any key length");
    mac.update(TOKEN_CONTEXT);
    mac.update(peer_id.as_bytes());
    mac.update(nonce.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// Compares two proofs without leaking, through timing, how much matched.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The desk's side of the handshake: who is allowed in, and who is not.
///
/// Holds no sockets. It is handed what arrived and says what to do about it,
/// which is what lets every rule below be tested without a network.
pub struct Doorkeeper {
    known: HashMap<String, Pairing>,
    offered: Option<Offer>,
}

struct Offer {
    code: String,
    made: Instant,
    failures: u32,
}

impl Doorkeeper {
    pub fn new(known: Vec<Pairing>) -> Self {
        Self {
            known: known.into_iter().map(|p| (p.peer_id.clone(), p)).collect(),
            offered: None,
        }
    }

    /// Starts offering a code, and returns it for the operator to read out.
    ///
    /// Offering again replaces the previous one and clears the failure count:
    /// pressing the button is how somebody who mistyped gets unstuck, and a
    /// fresh code makes the earlier attempts meaningless anyway.
    pub fn offer(&mut self) -> String {
        let code = new_code();
        self.offered = Some(Offer { code: code.clone(), made: Instant::now(), failures: 0 });
        code
    }

    /// Stops offering. Pairing should not be open for longer than somebody is
    /// standing at the other machine.
    pub fn withdraw(&mut self) {
        self.offered = None;
    }

    pub fn pairing_open(&self, now: Instant) -> bool {
        self.offered.as_ref().is_some_and(|offer| {
            now.duration_since(offer.made) < CODE_LIFETIME && offer.failures < MAX_ATTEMPTS
        })
    }

    pub fn challenge(&self, nonce: String, desk: &str, now: Instant) -> Challenge {
        Challenge { nonce, pairing_open: self.pairing_open(now), desk: desk.to_string() }
    }

    /// Everything remembered, for the product to persist.
    pub fn pairings(&self) -> Vec<Pairing> {
        let mut all: Vec<Pairing> = self.known.values().cloned().collect();
        all.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
        all
    }

    /// Forgets one screen. The operator's way of revoking a machine that has
    /// left the building.
    pub fn forget(&mut self, peer_id: &str) -> bool {
        self.known.remove(peer_id).is_some()
    }

    /// Judges an answer.
    ///
    /// `nonce` must be the one sent in the challenge this is answering, and
    /// must not be reused — that is what makes a captured proof worthless.
    pub fn admit(&mut self, answer: &Answer, nonce: &str, now: Instant) -> Verdict {
        match answer {
            Answer::Hello { id, proof } => match self.known.get(id) {
                Some(pairing) if same(proof, &prove(&pairing.token, nonce)) => {
                    Verdict::Ready { you: pairing.name.clone() }
                }
                // Both cases give the same answer on purpose. "I know that
                // machine but not that proof" tells anybody asking which ids
                // are worth attacking.
                _ => Verdict::Refused {
                    reason: "This machine is not paired with that desk. Pair it again.".into(),
                },
            },

            Answer::Pair { id, name, proof } => {
                if !self.pairing_open(now) {
                    return Verdict::Refused {
                        reason: "That desk is not offering a code just now. Ask for a new one."
                            .into(),
                    };
                }

                // Unwrapped safely: pairing_open above proved there is one.
                let offer = self.offered.as_mut().expect("pairing is open");
                if !same(proof, &prove(&offer.code, nonce)) {
                    offer.failures += 1;
                    let left = MAX_ATTEMPTS.saturating_sub(offer.failures);
                    return Verdict::Refused {
                        reason: if left == 0 {
                            "That code was wrong too many times. Ask the desk for a new one.".into()
                        } else {
                            format!("That code is not right. {left} more tries.")
                        },
                    };
                }

                // The token neither end will ever transmit.
                let token = derive_token(&offer.code, id, nonce);
                self.known.insert(
                    id.clone(),
                    Pairing { peer_id: id.clone(), name: name.clone(), token },
                );

                // A code pairs one machine. Leaving it open would let anyone
                // who overheard it pair a second, and the operator has no
                // reason to expect that a code they read out once still works.
                self.offered = None;

                Verdict::Ready { you: name.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Instant {
        Instant::now() + Duration::from_secs(seconds)
    }

    #[test]
    fn a_code_avoids_the_characters_people_confuse() {
        // Somebody is reading this off one screen and typing it into another,
        // in a hall, possibly in the dark.
        for _ in 0..200 {
            let code = new_code();
            assert_eq!(code.len(), CODE_LENGTH);
            for c in code.chars() {
                assert!(!"01OIL".contains(c), "{code} contains a character that gets mistyped");
                assert!(ALPHABET.contains(&(c as u8)), "{code} left the alphabet");
            }
        }
    }

    #[test]
    fn a_typed_code_is_read_however_it_was_typed() {
        assert_eq!(normalise("abc234"), "ABC234");
        assert_eq!(normalise("ABC-234"), "ABC234");
        assert_eq!(normalise(" a b c 2 3 4 "), "ABC234");
    }

    #[test]
    fn there_is_nothing_to_map_a_confusable_character_to() {
        // This is a note against a fix that looks obvious and is not. An
        // earlier version mapped O to 0 and I/L to 1 -- but the alphabet has
        // neither member of either pair, so both sides of every mapping are
        // characters a code can never contain. Somebody typing an O has
        // misread something else, and nothing can guess what.
        for c in "OIL01".chars() {
            assert!(!ALPHABET.contains(&(c as u8)), "{c} should never be issued");
        }
        // So it is passed through unchanged and simply does not match, which
        // is the truthful outcome.
        assert_eq!(normalise("o0i1l"), "O0I1L");
    }

    #[test]
    fn the_secret_never_appears_in_what_is_sent() {
        // The whole argument for challenge-response. A proof on the wire must
        // not carry the code that produced it.
        let nonce = new_nonce();
        let proof = prove("ABC234", &nonce);
        assert!(!proof.contains("ABC234"));
        assert_ne!(proof, "ABC234");
        // And a different nonce gives a different proof, or a capture would be
        // worth replaying.
        assert_ne!(proof, prove("ABC234", &new_nonce()));
    }

    #[test]
    fn both_ends_derive_the_same_token_without_sending_it() {
        let nonce = new_nonce();
        let desk = derive_token("ABC234", "screen-1", &nonce);
        let screen = derive_token("ABC234", "screen-1", &nonce);
        assert_eq!(desk, screen);

        // And nothing else derives it: not another code, another machine, or
        // another connection.
        assert_ne!(desk, derive_token("ABC235", "screen-1", &nonce));
        assert_ne!(desk, derive_token("ABC234", "screen-2", &nonce));
        assert_ne!(desk, derive_token("ABC234", "screen-1", &new_nonce()));
    }

    #[test]
    fn a_proof_of_one_thing_is_not_a_proof_of_another() {
        // Domain separation. Without it, a token derived from a code and a
        // proof of that same code are computed over the same input, and one
        // could stand in for the other.
        let nonce = new_nonce();
        assert_ne!(prove("ABC234", &nonce), derive_token("ABC234", "", &nonce));
    }

    #[test]
    fn pairing_admits_the_machine_somebody_walked_the_code_to() {
        let mut door = Doorkeeper::new(Vec::new());
        let code = door.offer();
        let nonce = new_nonce();

        let verdict = door.admit(
            &Answer::Pair {
                id: "screen-1".into(),
                name: "Stream machine".into(),
                proof: prove(&code, &nonce),
            },
            &nonce,
            Instant::now(),
        );

        assert_eq!(verdict, Verdict::Ready { you: "Stream machine".into() });
        assert_eq!(door.pairings().len(), 1);

        // And is remembered: the next connection proves the token instead, and
        // no code is offered any more.
        let later = new_nonce();
        let token = door.pairings()[0].token.clone();
        assert_eq!(
            door.admit(
                &Answer::Hello { id: "screen-1".into(), proof: prove(&token, &later) },
                &later,
                Instant::now(),
            ),
            Verdict::Ready { you: "Stream machine".into() },
        );
    }

    #[test]
    fn one_code_pairs_one_machine() {
        // Anybody within earshot heard the code read out. The operator has no
        // reason to expect it still works afterwards, so it does not.
        let mut door = Doorkeeper::new(Vec::new());
        let code = door.offer();
        let nonce = new_nonce();
        door.admit(
            &Answer::Pair { id: "screen-1".into(), name: "One".into(), proof: prove(&code, &nonce) },
            &nonce,
            Instant::now(),
        );

        let second = new_nonce();
        let verdict = door.admit(
            &Answer::Pair {
                id: "screen-2".into(),
                name: "Two".into(),
                proof: prove(&code, &second),
            },
            &second,
            Instant::now(),
        );
        assert!(matches!(verdict, Verdict::Refused { .. }));
        assert_eq!(door.pairings().len(), 1);
    }

    #[test]
    fn guessing_is_shut_down_before_it_gets_anywhere() {
        let mut door = Doorkeeper::new(Vec::new());
        door.offer();

        for _ in 0..MAX_ATTEMPTS {
            let nonce = new_nonce();
            let verdict = door.admit(
                &Answer::Pair {
                    id: "intruder".into(),
                    name: "?".into(),
                    proof: prove("WRONG1", &nonce),
                },
                &nonce,
                Instant::now(),
            );
            assert!(matches!(verdict, Verdict::Refused { .. }));
        }

        assert!(!door.pairing_open(Instant::now()), "should have closed after the attempts");

        // Offering again is how the operator gets unstuck, and it clears the
        // count -- the old code is meaningless now anyway.
        door.offer();
        assert!(door.pairing_open(Instant::now()));
    }

    #[test]
    fn a_code_left_on_a_screen_goes_stale() {
        let mut door = Doorkeeper::new(Vec::new());
        let code = door.offer();
        assert!(door.pairing_open(Instant::now()));
        assert!(!door.pairing_open(at(CODE_LIFETIME.as_secs() + 1)));

        let nonce = new_nonce();
        let verdict = door.admit(
            &Answer::Pair { id: "late".into(), name: "?".into(), proof: prove(&code, &nonce) },
            &nonce,
            at(CODE_LIFETIME.as_secs() + 1),
        );
        assert!(matches!(verdict, Verdict::Refused { .. }));
    }

    #[test]
    fn a_captured_proof_is_worth_nothing_on_the_next_connection() {
        // The reason the nonce is fresh every time.
        let mut door = Doorkeeper::new(Vec::new());
        let code = door.offer();
        let first = new_nonce();
        let captured = prove(&code, &first);
        door.admit(
            &Answer::Pair { id: "screen-1".into(), name: "One".into(), proof: captured.clone() },
            &first,
            Instant::now(),
        );

        let second = new_nonce();
        let verdict = door.admit(
            &Answer::Hello { id: "screen-1".into(), proof: captured },
            &second,
            Instant::now(),
        );
        assert!(matches!(verdict, Verdict::Refused { .. }));
    }

    #[test]
    fn an_unknown_machine_and_a_bad_proof_are_refused_the_same_way() {
        // Telling them apart would say which ids exist, which is the list an
        // attacker would want first.
        let mut door = Doorkeeper::new(vec![Pairing {
            peer_id: "known".into(),
            name: "Known".into(),
            token: "a-token".into(),
        }]);
        let nonce = new_nonce();

        let unknown = door.admit(
            &Answer::Hello { id: "stranger".into(), proof: prove("a-token", &nonce) },
            &nonce,
            Instant::now(),
        );
        let wrong = door.admit(
            &Answer::Hello { id: "known".into(), proof: prove("guessed", &nonce) },
            &nonce,
            Instant::now(),
        );
        assert_eq!(unknown, wrong);
    }

    #[test]
    fn forgetting_a_machine_shuts_it_out() {
        let mut door = Doorkeeper::new(vec![Pairing {
            peer_id: "old".into(),
            name: "Sold laptop".into(),
            token: "a-token".into(),
        }]);
        assert!(door.forget("old"));
        assert!(!door.forget("old"));

        let nonce = new_nonce();
        let verdict = door.admit(
            &Answer::Hello { id: "old".into(), proof: prove("a-token", &nonce) },
            &nonce,
            Instant::now(),
        );
        assert!(matches!(verdict, Verdict::Refused { .. }));
    }
}
