"""slwlab.merge_dryrun: chunk merging, coverage by UTC hour, gaps flagged and never filled."""

import json

import pytest

from slwlab import merge_dryrun as md

H = 3_600 * 10**9
T0 = 1_792_713_600 * 10**9  # 2026-10-23T00:00:00Z


def iso(ns):
    return md.fmt_ns(ns)


def make_run(logs, stamp, start, end, *, host=None, gaps=(), errs=(), summary=True, torn=False,
             endpoint="https://mainnet.helius-rpc.com/?api-key=REDACTED", cluster="mainnet", purpose=None):
    d = logs / (f"live-{stamp}" if purpose else f"dryrun-{stamp}")
    d.mkdir(parents=True)
    run = {"cluster": cluster, "mode": "live" if purpose else "dry_run", "purpose": purpose,
           "started": iso(start), "run_config": {"endpoint": endpoint}}
    if host:
        run["host"] = host
    (d / "run.json").write_text(json.dumps(run))
    if summary:
        (d / "summary.json").write_text(json.dumps({"finished": iso(end)}))
    (d / "gaps.jsonl").write_text("".join(json.dumps(
        {"kind": k, "start_unix_ns": a, "end_unix_ns": b, "secs": (b - a) / 1e9}) + "\n" for a, b, k in gaps))
    lines = [json.dumps({"predicted_prep_unix_ns": t, "observed_unix_ns": t + 1, "err_prep_ms": e}) for t, e in errs]
    (d / "clock.jsonl").write_text("\n".join(lines) + ("\n{\"predicted_prep" if torn else "\n" if lines else ""))
    return d


PC = {"platform": "win-pc1", "os": "windows", "machine_id": "0123456789abcdef", "os_build": "10.0.17763.6893",
      "sender_sha256": "ab" * 32}


def test_merge_two_platforms_coverage_and_flags(tmp_path):
    logs = tmp_path / "logs"
    # first chunk 08:00-10:00 with a 40 min host_stall at 09:00
    make_run(logs, "20261023T080000Z", T0 + 8 * H, T0 + 10 * H,
             gaps=[(T0 + 9 * H, T0 + 9 * H + 2 * H // 3, "host_stall")],
             errs=[(T0 + 8 * H + 10**9, 50.0), (T0 + 8 * H + 2 * 10**9, -150.0)])
    # second chunk 20:00-22:20, died without summary: end inferred from its last clock record
    make_run(logs, "20261023T200000Z", T0 + 20 * H, T0 + 23 * H, host=PC, summary=False, torn=True,
             errs=[(T0 + 20 * H + 10**9, 80.0), (T0 + 22 * H + H // 3, 20.0)])
    # a devnet run in range is ignored
    make_run(logs, "20261023T090000Z", T0 + 9 * H, T0 + 10 * H, cluster="devnet")
    m = md.merge(tmp_path, "20261023T000000Z", None)
    assert [r["end_inferred"] for r in m["runs"]] == [False, True]
    assert m["runs"][1]["torn_lines"] == 1
    assert set(m["by_platform"]) == {"dry_run | unrecorded via mainnet.helius-rpc.com",
                                     "dry_run | win-pc1/windows/0123456789abcdef via mainnet.helius-rpc.com"}
    pooled = {r["hour_of_day_utc"]: r for r in m["pooled"]["by_hour_of_day"]}
    assert pooled["08"]["flag"] == "OK" and pooled["08"]["covered_secs"] == 3600
    assert pooled["09"]["covered_secs"] == 1200 and pooled["09"]["flag"] == "THIN"
    assert pooled["22"]["covered_secs"] == pytest.approx(1200, abs=1)
    assert pooled["22"]["flag"] == "THIN"
    assert pooled["23"]["flag"] == "GAP" and pooled["23"]["err"] is None
    assert pooled["03"]["flag"] == "GAP" and pooled["03"]["dates"] == []
    assert pooled["08"]["err"]["n"] == 2 and pooled["08"]["err"]["median_abs_ms"] == 100.0
    # never filled: hours between the chunks exist in the timeline with zero coverage and no stats
    hours = {r["hour_utc"]: r for r in m["pooled"]["hours"]}
    assert hours["2026-10-23T15:00:00Z"]["covered_secs"] == 0
    assert hours["2026-10-23T15:00:00Z"]["err"] is None
    assert hours["2026-10-23T09:00:00Z"]["blind_secs"] == {"host_stall": 2400.0}
    assert m["platform_overlap"] == []
    assert "GAP" in md.markdown(m)


def test_overlapping_groups_are_reported_not_pooled_away(tmp_path):
    logs = tmp_path / "logs"
    make_run(logs, "20261023T100000Z", T0 + 10 * H, T0 + 12 * H, host=PC)
    make_run(logs, "20261023T110000Z", T0 + 11 * H, T0 + 13 * H, host=PC, purpose="calibrate")
    ev = tmp_path / "sessions" / "20261023T095959Z"
    ev.mkdir(parents=True)
    (ev / "events.jsonl").write_text(json.dumps({"ts": "2026-10-23T10:00:00Z", "event": "start"}) + "\n")
    m = md.merge(tmp_path, "20261023T000000Z", None)
    assert set(m["by_platform"]) == {"dry_run | win-pc1/windows/0123456789abcdef via mainnet.helius-rpc.com",
                                     "live-calibrate | win-pc1/windows/0123456789abcdef via mainnet.helius-rpc.com"}
    assert m["campaign_events"][0]["file"] == "sessions/20261023T095959Z/events.jsonl"
    assert m["platform_overlap"][0]["secs"] == 3600
    # the pooled live time is the union, not the sum
    assert m["pooled"]["covered_secs_total"] == 3 * 3600


def test_bad_stamp_and_empty_range(tmp_path):
    (tmp_path / "logs").mkdir()
    with pytest.raises(SystemExit, match="compact UTC"):
        md.merge(tmp_path, "2026-10-23", None)
    with pytest.raises(SystemExit, match="no mainnet dry-run"):
        md.merge(tmp_path, "20261023T000000Z", None)
