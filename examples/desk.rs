// A standalone Pulpitry desk, to prove the two products actually talk.
fn main() {
    let desk = castavox_core::mirror::Desk::start("Test Desk", Vec::new(), 7854).unwrap();
    let _advert = castavox_core::mirror::advertise(
        "test-desk-id", "Test Desk", "pulpitry", "0.4.0", desk.port(),
    ).unwrap();
    println!("CODE {}", desk.offer_code());

    // Stage a verse a few seconds in, so the screen has to receive a change
    // rather than only the state it joined into.
    std::thread::sleep(std::time::Duration::from_secs(20));
    desk.stage(castavox_core::mirror::Shown::Scripture {
        reference: "John 3:16".into(),
        translation: "KJV".into(),
        lines: vec![castavox_core::mirror::Line {
            number: 16,
            text: "For God so loved the world, that he gave his only begotten Son.".into(),
        }],
    });
    println!("STAGED");
    std::thread::sleep(std::time::Duration::from_secs(120));
}
