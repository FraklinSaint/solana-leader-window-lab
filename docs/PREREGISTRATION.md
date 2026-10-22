# Pre-registration (DRAFT, not yet frozen)

This document will be frozen, committed and tagged `prereg-v1` in **Phase 4, before any live
mainnet data is collected**. Until then, it is a draft. After the tag, thresholds do not change.
Any deviation appears in REPORT.md as a labeled deviation.

Sections to be finalized in Phases 1 and 4: candidate rules; per-window statistic (Kendall tau-b
plus an exact permutation test); per validator × arm aggregation (window bootstrap, Holm
correction, paired bootstrap between the top two rules); strictness; verdict thresholds
(STRICT_RULE / SOFT_RULE / NO_RULE / INCONCLUSIVE); the definition of a usable window;
exclusions; controls (positive, label-shuffle, simulator false-positive rate and power);
sensitivity analyses.

Source-derived predictions already registered in docs/VERIFIED_FACTS.md F6:
- Agave-family: position ≈ R_RATIO order, degraded by parallel-thread interleaving;
  equal-priority ties are NOT in send order.
- Firedancer-family: R_RATIO_FD (exact rational comparison); same predicted permutation as
  R_RATIO_AGAVE for our arms.
