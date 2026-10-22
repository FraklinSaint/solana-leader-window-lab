//! Spend limits. The hard ceiling is a compile-time constant; the CLI can only lower it.

/// Absolute maximum the whole project may spend on fees: 0.05 SOL (bounty requirement).
pub const HARD_CEILING_LAMPORTS: u64 = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapError {
    /// `--max-spend-lamports` was not given; live mode requires an explicit cap.
    Missing,
    /// The requested cap exceeds the compile-time ceiling.
    AboveCeiling { requested: u64, ceiling: u64 },
    /// A zero cap would make every send fail; reject it up front.
    Zero,
}

impl std::fmt::Display for CapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapError::Missing => write!(f, "live mode requires an explicit --max-spend-lamports"),
            CapError::AboveCeiling { requested, ceiling } => write!(
                f,
                "--max-spend-lamports {requested} exceeds HARD_CEILING_LAMPORTS {ceiling}"
            ),
            CapError::Zero => write!(f, "--max-spend-lamports must be > 0"),
        }
    }
}

impl std::error::Error for CapError {}

/// Resolve the effective spend cap. Rejects (does not clamp) values above the ceiling,
/// so a typo can never silently turn into "spend the maximum".
pub fn effective_cap(requested: Option<u64>) -> Result<u64, CapError> {
    match requested {
        None => Err(CapError::Missing),
        Some(0) => Err(CapError::Zero),
        Some(r) if r > HARD_CEILING_LAMPORTS => Err(CapError::AboveCeiling {
            requested: r,
            ceiling: HARD_CEILING_LAMPORTS,
        }),
        Some(r) => Ok(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_is_required() {
        assert_eq!(effective_cap(None), Err(CapError::Missing));
    }

    #[test]
    fn cap_above_ceiling_is_rejected_not_clamped() {
        assert!(matches!(
            effective_cap(Some(HARD_CEILING_LAMPORTS + 1)),
            Err(CapError::AboveCeiling { .. })
        ));
    }

    #[test]
    fn cap_at_or_below_ceiling_is_accepted() {
        assert_eq!(
            effective_cap(Some(HARD_CEILING_LAMPORTS)),
            Ok(HARD_CEILING_LAMPORTS)
        );
        assert_eq!(effective_cap(Some(5_000_000)), Ok(5_000_000));
    }

    #[test]
    fn zero_cap_is_rejected() {
        assert_eq!(effective_cap(Some(0)), Err(CapError::Zero));
    }
}
