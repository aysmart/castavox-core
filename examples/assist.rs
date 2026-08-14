//! Reproduces exactly what a summary does, and prints the cause reqwest hides.
fn main() {
    castavox_core::tls::install();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("client");

    let token = std::env::args().nth(1).unwrap_or_default();
    let result = client
        .post("https://www.castavox.com/api/v1/assist")
        .bearer_auth(token)
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": "say ok"}],
            "max_tokens": 10
        }))
        .send();

    match result {
        Ok(response) => println!("status {}", response.status()),
        Err(error) => {
            println!("error: {error}");
            let mut source: Option<&dyn std::error::Error> = std::error::Error::source(&error);
            while let Some(cause) = source {
                println!("  caused by: {cause}");
                source = cause.source();
            }
        }
    }
}
