//! One error type at the CLI boundary (M0-5, §16): every fallible step in `main`
//! collapses into [`MateError`] before it's printed, so failures get a stable, category-scoped
//! exit code instead of leaking as an opaque `anyhow::Error` dump.
//!
//! Everything below this layer stays `anyhow::Result` (the binary edge, per the error
//! strategy) — `main` pins each call site's `anyhow::Error` to a `MateError` variant with
//! `.map_err(...)`, right where it knows what failed.

/// Exit-code-bearing error at the CLI boundary. One variant per category `main` can act on
/// distinctly. `Auth` (missing `API_TOKEN`, or a provider auth rejection classified by
/// `ProviderError::classify`) and `Provider` (every other provider-request failure) are
/// produced by the plain frontend's startup and turn-running paths (`M5-4`).
#[derive(Debug, thiserror::Error)]
pub enum MateError {
    #[error("config error: {0}")]
    Config(#[source] anyhow::Error),

    #[error("io error: {0}")]
    Io(#[source] anyhow::Error),

    #[error("authentication error: {0}")]
    Auth(#[source] anyhow::Error),

    #[error("provider error: {0}")]
    Provider(#[source] anyhow::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl MateError {
    /// Process exit code for this category. `0` is reserved for success and never returned
    /// here.
    pub fn exit_code(&self) -> u8 {
        match self {
            MateError::Config(_) => 2,
            MateError::Io(_) => 3,
            MateError::Auth(_) => 4,
            MateError::Provider(_) => 5,
            MateError::Other(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_has_a_distinct_nonzero_exit_code() {
        let codes = [
            MateError::Config(anyhow::anyhow!("x")).exit_code(),
            MateError::Io(anyhow::anyhow!("x")).exit_code(),
            MateError::Auth(anyhow::anyhow!("x")).exit_code(),
            MateError::Provider(anyhow::anyhow!("x")).exit_code(),
            MateError::Other(anyhow::anyhow!("x")).exit_code(),
        ];
        assert!(codes.iter().all(|c| *c != 0));
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
    }
}
