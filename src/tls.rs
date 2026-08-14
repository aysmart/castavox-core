//! Choosing a TLS backend, once, before anything makes a request.
//!
//! rustls is built here without a default cryptography provider, so one has to
//! be installed before the first HTTPS client is constructed. Naming it
//! explicitly is the price of not compiling aws-lc-rs, which would drag a C
//! toolchain onto every machine that builds this.
//!
//! It lives in its own module because more than one thing needs it and the
//! order matters. It used to sit inside the assistant, which meant fetching a
//! speech model over HTTPS worked only if the operator happened to have
//! configured the assistant first -- and in Castavox, where that call was never
//! made at all, model downloads failed outright.
//!
//! Every entry point that builds an HTTPS client calls this first. It is cheap,
//! idempotent, and the failure it prevents is a panic on a background thread
//! that takes the request with it and explains nothing.

/// A client builder with the provider already installed.
///
/// # Why this exists rather than a rule to remember
///
/// reqwest is taken with `rustls-no-provider`, so building a `Client` before a
/// provider is installed does not fail — it **panics**, on whichever thread got
/// there first, with a message about aws-lc-rs that names neither the caller
/// nor the feature that caused it.
///
/// Every call site had to remember to call [`install`] first, and one of them
/// did not: the translation fetcher built its client at start-up, before
/// anything else had installed anything, and took the whole speech sidecar down
/// with it. Nothing about that call site looked wrong.
///
/// So the order is not a rule any more. Ask for a builder and it is already
/// safe; there is nothing left to forget.
pub fn client() -> reqwest::blocking::ClientBuilder {
    install();
    reqwest::blocking::Client::builder()
}

/// The same, for the asynchronous client.
pub fn async_client() -> reqwest::ClientBuilder {
    install();
    reqwest::Client::builder()
}

/// Installs the default provider. Safe to call from anywhere, any number of
/// times, from any thread.
///
/// Prefer [`client`], which cannot be called in the wrong order.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Ignored deliberately: an error here means somebody else installed one
        // first, which is the outcome we wanted.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn installing_twice_is_not_a_problem() {
        super::install();
        super::install();
    }

    #[test]
    fn a_client_can_be_built_without_anybody_installing_anything_first() {
        // The property that matters. With `rustls-no-provider`, building a
        // client before a provider exists panics rather than failing -- so a
        // call site that forgot took the whole process down, and nothing about
        // it looked wrong.
        super::client().build().expect("a client, without ceremony");
    }
}
