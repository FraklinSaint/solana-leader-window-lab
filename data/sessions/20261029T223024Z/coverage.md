# Session 20261029T223024Z (measure) coverage by UTC hour (EXPLORATORY)

2026-10-29T22:30:25Z to 2026-10-30T07:30:25Z. Covered 9.0 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-10-29T22:00:00Z | 30 | 30 |  | 15 | 473512 | 114 | OK |
| 2026-10-29T23:00:00Z | 60 | 60 |  | 30 | 949566 | 107 | OK |
| 2026-10-30T00:00:00Z | 60 | 60 |  | 29 | 912650 | 84 | OK |
| 2026-10-30T01:00:00Z | 60 | 60 |  | 30 | 950576 | 74 | OK |
| 2026-10-30T02:00:00Z | 60 | 60 |  | 29 | 915018 | 97 | OK |
| 2026-10-30T03:00:00Z | 60 | 60 |  | 30 | 952944 | 52 | OK |
| 2026-10-30T04:00:00Z | 60 | 60 |  | 29 | 915018 | 67 | OK |
| 2026-10-30T05:00:00Z | 60 | 60 |  | 25 | 794828 | 88 | OK |
| 2026-10-30T06:00:00Z | 60 | 60 |  | 19 | 603696 | 85 | OK |
| 2026-10-30T07:00:00Z | 30 | 30 |  | 10 | 318948 | 61 | OK |

Reserved in the ledger during the session: 7786756 lamports (cap 8410024). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| allocation quota met | 4670 |
| pacing: # + # > # (plus one window) | 1582 |
| too close to previous planned window | 226 |
| window truncated by schedule range | 45 |
| QUIC connect failed: QUIC handshake timed out (# ms): deadline has elapsed | 1 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 6376 probes, 0 failed: min 42.2 ms,          median 53.3, p95 124.0, p99 136.3, max 277.6; robust sigma 7.0 ms; jitter (mean |diff| of consecutive          probes) 17.5 ms.

         Wi-Fi: network types {"wifi":1063}, bands {"2.4 GHz":1063}, channels {"13":1063}, PHY {"802.11n":1063}, 1 access point(s), 0 change(s).          Signal quality min 88 / median 94 / max 100 %.
