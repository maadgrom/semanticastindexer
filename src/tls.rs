//! One-time process-level rustls `CryptoProvider` install.
//!
//! With `--features all`, feature unification turns on BOTH rustls crypto providers:
//! `ring` (via qdrant-client's tonic gRPC channel and hf-hub's reqwest) and `aws-lc-rs`
//! (via the ollama embedder's reqwest → `rustls-platform-verifier`). rustls 0.23 refuses
//! to auto-pick a process-level default when both (or neither) of these crate features
//! are enabled, and PANICS the first time a TLS `ClientConfig` is built:
//!
//! > Could not automatically determine the process-level CryptoProvider from Rustls crate
//! > features. Call CryptoProvider::install_default() before this point ...
//!
//! For the Qdrant backend that first `ClientConfig` is built off-thread inside the
//! client's health check, so the panic surfaces as `Failed to join health check thread`
//! and takes down the whole run. Installing a provider explicitly removes the ambiguity.

/// Install the process-wide rustls crypto provider exactly once, before any TLS client
/// (Qdrant gRPC, ollama/hf-hub HTTPS) is constructed. Call this first thing in `main`.
///
/// We pick `ring` because it is present in every TLS-using build configuration. The call
/// is idempotent and safe to make unconditionally: if a provider is already installed,
/// `install_default` returns `Err` and we ignore it. It is a no-op in builds without any
/// TLS-using feature (no rustls edge), so the binary never needs a backend to be selected
/// to call this.
pub fn install_crypto_provider() {
    #[cfg(any(feature = "qdrant", feature = "ort", feature = "ollama"))]
    {
        // Returns Err(already_installed_provider) on a second call — intentionally ignored.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
