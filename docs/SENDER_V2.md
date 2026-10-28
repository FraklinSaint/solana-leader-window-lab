# Sender v2 (expected binary from 2026-10-28)

This file is not part of the `prereg-v1` tag. Where it differs from `docs/RUNBOOK_MAINNET.md`
(which stays as it was in the tag), this file applies.

## Expected binary

For every session from 2026-10-28 on the expected binary is **sender v2** = sender v1 plus the
stale-TPU refresh (the idle wait before a window's prep re-reads gossip `tpuQuic` of the selected
validators when the cache is older than 60 s; send protocol, timing and order unchanged).

| | SHA-256 | Short |
|---|---|---|
| v2 (expected) | `475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718` | `475fe47a880a` |
| v1 (`prereg-v1`) | `903525409e18f5ad4aedfaffcdc2fb60e3b63ff0fc596ae834769469e4687317` | `903525409e18` |

Copies: `D:\slwlab-backup\sender_v2_stale_tpu.exe` (v2), `D:\slwlab-backup\sender_prereg_v1.exe` (v1).

## Expected start line

```
starting measure session <id> until <UTC end> (sender 475fe47a880a)
```

(`calibrate` or `dryrun` in place of `measure` for those modes.)

## Where this file overrides the runbook

`docs/RUNBOOK_MAINNET.md` shows the v1 hash at lines 67, 255, 333, 352 and 366, and in the start
line at 454. Wherever it does, the v2 hash above replaces it (full hash for the `Get-FileHash`
checks at 67 and 366, `(sender 475fe47a880a)` for the start lines at 255, 333 and 454, and "final"
at 352 means v2). `docs/RUNBOOK_2026-10-27.md` and the `SESSION_*_COMMANDS.txt` files are
historical and still name v1.

## Where v1 applies

The v1 hash `903525409e18` applies to the calibration and to the sessions of 2026-10-26 and
2026-10-27 (session 1 `live-20261026T220348Z`, session 2 `live-20261027T214442Z`).

## References

- LOGBOOK entry "Decision and change, stale TPU address" (2026-10-28 12:13 UTC) and the dry-run
  check of the same day.
- Patch: commit `6de3edb`. Dry-run of v2 with no sends: commit `f45025d`
  (`dryrun-20261028T121200Z`).
- The binary hash is recorded in every run (`host.sender_sha256`), not gated by the sender.
