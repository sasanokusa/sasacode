//! The shared HTTP client. Every request to a model API or a web page goes through [`client`]: it
//! installs the TLS crypto provider (ring) once, which reqwest needs before the first connection.

use std::sync::OnceLock;

/// Install the process-wide rustls crypto provider (idempotent). Call before building any other
/// reqwest client.
pub fn init_tls() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A new client builder with TLS ready. Use it for clients with their own settings (redirects, timeouts).
pub fn builder() -> reqwest::ClientBuilder {
    init_tls();
    reqwest::Client::builder()
}

/// The shared client: no overall timeout (streams can be long), connection pooling on.
pub fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| builder().build().unwrap_or_else(|_| reqwest::Client::new()))
}
