//! The live gate. A [`LiveAuth`] can only be obtained through [`authorize`], and every code
//! path that writes transaction bytes to the network takes `&LiveAuth`. Dry-run code never
//! holds one, so it cannot send — this is enforced by the type system, not by a flag check
//! deep inside the sender.

use slwlab_common::limits::{CapError, effective_cap};

pub const CONFIRM_ENV: &str = "CONFIRM_LIVE";

#[derive(Debug)]
pub struct LiveAuth {
    cap: u64,
}

impl LiveAuth {
    pub fn cap(&self) -> u64 {
        self.cap
    }

    /// Test-only constructor so transport tests can exercise the live path against a local
    /// in-process server.
    #[cfg(test)]
    pub(crate) fn for_tests(cap: u64) -> Self {
        Self { cap }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveError {
    #[error("live sending requires the `live` subcommand")]
    NotRequested,
    #[error("live sending requires env {CONFIRM_ENV}=yes (got {0:?})")]
    NotConfirmed(Option<String>),
    #[error(transparent)]
    Cap(#[from] CapError),
}

/// All three are required: the `live` subcommand, `CONFIRM_LIVE=yes`, and an explicit cap
/// that does not exceed the compile-time ceiling.
pub fn authorize(
    live_requested: bool,
    confirm_env: Option<&str>,
    max_spend_lamports: Option<u64>,
) -> Result<LiveAuth, LiveError> {
    if !live_requested {
        return Err(LiveError::NotRequested);
    }
    if confirm_env != Some("yes") {
        return Err(LiveError::NotConfirmed(confirm_env.map(str::to_string)));
    }
    let cap = effective_cap(max_spend_lamports)?;
    Ok(LiveAuth { cap })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slwlab_common::limits::HARD_CEILING_LAMPORTS;

    #[test]
    fn every_gate_is_required() {
        assert_eq!(
            authorize(false, Some("yes"), Some(1)).unwrap_err(),
            LiveError::NotRequested
        );
        assert!(matches!(
            authorize(true, None, Some(1)).unwrap_err(),
            LiveError::NotConfirmed(None)
        ));
        assert!(matches!(
            authorize(true, Some("YES"), Some(1)).unwrap_err(),
            LiveError::NotConfirmed(_)
        ));
        assert_eq!(
            authorize(true, Some("yes"), None).unwrap_err(),
            LiveError::Cap(CapError::Missing)
        );
        assert!(matches!(
            authorize(true, Some("yes"), Some(HARD_CEILING_LAMPORTS + 1)).unwrap_err(),
            LiveError::Cap(CapError::AboveCeiling { .. })
        ));
        assert_eq!(
            authorize(true, Some("yes"), Some(5_000_000)).unwrap().cap(),
            5_000_000
        );
    }
}
