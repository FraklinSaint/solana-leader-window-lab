//! Alpenglow boundary guard. Measurements before the boundary are never planned,
//! sent or analyzed; the boundary is defined by slot, not wall-clock time.

/// First slot under Alpenglow (Votor) on mainnet. Verified on-chain: slot 459_652_999 is the
/// last block containing vote transactions, slots 459_653_000..=459_653_007 were skipped and
/// 459_653_008 is the first produced block without vote txs (docs/VERIFIED_FACTS.md, F1).
pub const ALPENGLOW_GENESIS_SLOT: u64 = 459_653_000;

/// Epoch containing the boundary.
pub const ALPENGLOW_EPOCH: u64 = 1064;

/// Mainnet epoch length (getEpochSchedule, warmup=false). Planner re-checks against RPC.
pub const SLOTS_PER_EPOCH: u64 = 432_000;

/// Default stabilization margin: live measurements start no earlier than this epoch.
pub const DEFAULT_MIN_LIVE_EPOCH: u64 = 1065;

pub const fn first_slot_of_epoch(epoch: u64) -> u64 {
    epoch * SLOTS_PER_EPOCH
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardError {
    PreAlpenglow { slot: u64 },
    BeforeStabilization { slot: u64, min_epoch: u64 },
    MinEpochBeforeAlpenglow { min_epoch: u64 },
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::PreAlpenglow { slot } => write!(
                f,
                "slot {slot} is before ALPENGLOW_GENESIS_SLOT {ALPENGLOW_GENESIS_SLOT}; refusing"
            ),
            GuardError::BeforeStabilization { slot, min_epoch } => write!(
                f,
                "slot {slot} is before the stabilization epoch {min_epoch} (first slot {})",
                first_slot_of_epoch(*min_epoch)
            ),
            GuardError::MinEpochBeforeAlpenglow { min_epoch } => write!(
                f,
                "min live epoch {min_epoch} is before the Alpenglow epoch {ALPENGLOW_EPOCH}"
            ),
        }
    }
}

impl std::error::Error for GuardError {}

/// Any slot used for analysis must be on the Alpenglow side of the boundary.
pub fn check_post_alpenglow(slot: u64) -> Result<(), GuardError> {
    if slot < ALPENGLOW_GENESIS_SLOT {
        Err(GuardError::PreAlpenglow { slot })
    } else {
        Ok(())
    }
}

/// Slots we plan or send into must additionally be past the stabilization margin.
pub fn check_live_slot(slot: u64, min_live_epoch: u64) -> Result<(), GuardError> {
    if min_live_epoch < ALPENGLOW_EPOCH {
        return Err(GuardError::MinEpochBeforeAlpenglow {
            min_epoch: min_live_epoch,
        });
    }
    check_post_alpenglow(slot)?;
    if slot < first_slot_of_epoch(min_live_epoch) {
        return Err(GuardError::BeforeStabilization {
            slot,
            min_epoch: min_live_epoch,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_is_inclusive() {
        assert!(check_post_alpenglow(ALPENGLOW_GENESIS_SLOT).is_ok());
        assert_eq!(
            check_post_alpenglow(ALPENGLOW_GENESIS_SLOT - 1),
            Err(GuardError::PreAlpenglow {
                slot: ALPENGLOW_GENESIS_SLOT - 1
            })
        );
    }

    #[test]
    fn boundary_lies_in_epoch_1064() {
        assert_eq!(ALPENGLOW_GENESIS_SLOT / SLOTS_PER_EPOCH, ALPENGLOW_EPOCH);
    }

    #[test]
    fn stabilization_margin_applies_to_live_slots() {
        let first_1065 = first_slot_of_epoch(1065);
        assert_eq!(first_1065, 460_080_000);
        assert!(check_live_slot(first_1065, DEFAULT_MIN_LIVE_EPOCH).is_ok());
        assert!(matches!(
            check_live_slot(first_1065 - 1, DEFAULT_MIN_LIVE_EPOCH),
            Err(GuardError::BeforeStabilization { .. })
        ));
        // A post-Alpenglow slot inside epoch 1064 is analyzable but not live-plannable.
        assert!(check_post_alpenglow(459_700_000).is_ok());
        assert!(check_live_slot(459_700_000, DEFAULT_MIN_LIVE_EPOCH).is_err());
    }

    #[test]
    fn min_epoch_cannot_precede_alpenglow() {
        assert!(matches!(
            check_live_slot(500_000_000, 1000),
            Err(GuardError::MinEpochBeforeAlpenglow { .. })
        ));
    }
}
