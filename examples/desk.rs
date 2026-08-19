//! A desk, without Pulpitry, for testing the other half of the mirror.
//!
//!     cargo run --example desk
//!
//! Prints a pairing code, advertises itself as a Pulpitry desk, stages a verse
//! twenty seconds in, and beats for two minutes. Point a real Castavox sidecar
//! at it to exercise discovery, pairing, delivery and reconnection without
//! needing two applications and two machines.
//!
//! Worth keeping: running the two products against each other is what found the
//! two bugs the unit tests could not, because both ends were internally
//! consistent and only disagreed about what a field meant.

fn main() {
    let desk = castavox_core::mirror::Desk::start("Test Desk", Vec::new(), 7854).unwrap();
    let _advert = castavox_core::mirror::advertise(
        "test-desk-id",
        "Test Desk",
        "pulpitry",
        "0.4.0",
        desk.port(),
    )
    .unwrap();

    println!("CODE {}", desk.offer_code());

    // Staged late on purpose: a screen that pairs first has to receive a
    // *change*, not only the state it joined into.
    std::thread::sleep(std::time::Duration::from_secs(20));
    desk.stage(
        castavox_core::mirror::Shown::Scripture {
            reference: "John 3:16".into(),
            translation: "KJV".into(),
            lines: vec![castavox_core::mirror::Line {
                number: 16,
                text: "For God so loved the world, that he gave his only begotten Son.".into(),
            }],
        },
        // From the top: a single verse fits any screen worth mirroring to.
        0.0,
    );
    println!("STAGED");

    // Without this the screen's read times out and a quiet service looks like
    // a dead desk -- which is how the heartbeat earned its place.
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        desk.beat();
    }
}
