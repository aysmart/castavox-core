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

/// Installs the default provider. Safe to call from anywhere, any number of
/// times, from any thread.
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
}
