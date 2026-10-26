"""PREREGISTRATION §9: windows with a skipped slot are classified from the stored getBlocks
list (slwlab.blocklist), never from our transactions. Synthetic fixtures only."""
import json

import pytest

from slwlab import analyze, blocklist, parse, sim

from test_parse_analyze import SIM_REF, synth


def _present(lines, missing_by_window=None):
    """window_id -> all 4 slots present, except the ones named in `missing_by_window`."""
    missing_by_window = missing_by_window or {}
    out = {}
    for l in lines:
        t = l["tx"]
        slots = set(range(t["first_slot"], t["first_slot"] + t["n_slots"]))
        out[t["window_id"]] = slots - set(missing_by_window.get(t["window_id"], ()))
    return out


def test_skipped_slot_excludes_the_window_whatever_our_txs_did(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=12)
    base = {w.window_id: w for w in parse.windows_from_manifest(lines)}
    target = next(k for k, w in base.items() if w.usable)
    # the skipped slot is one of the window's slots that none of our txs landed in
    miss = {target: [base[target].first_slot + 3]}
    ws = {w.window_id: w for w in parse.windows_from_manifest(lines, _present(lines, miss))}
    assert ws[target].reason == "skipped_slot" and not ws[target].usable
    assert ws[target].skipped == [base[target].first_slot + 3] and ws[target].slot_status == "skipped"
    for k, w in ws.items():  # every other window is classified exactly as before
        if k != target:
            assert (w.usable, w.reason) == (base[k].usable, base[k].reason)


def test_sensitivity_keeps_skipped_windows_and_report_counts_them(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=12)
    base = {w.window_id: w for w in parse.windows_from_manifest(lines)}
    target = next(k for k, w in base.items() if w.usable)
    present = _present(lines, {target: [base[target].first_slot + 3]})
    kept = {w.window_id: w for w in parse.windows_from_manifest(lines, present, include_skipped=True)}
    assert kept[target].usable and kept[target].skipped
    rep = parse.skipped_report(list(kept.values()))
    (ident, r), = rep.items()
    assert r["windows"] == 12 and r["windows_with_skipped_slot"] == 1 and r["skipped_slots"] == 1
    assert r["share"] == pytest.approx(1 / 12)


def test_missing_list_is_never_guessed(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=5)
    ws = parse.windows_from_manifest(lines, {}, require_slot_status=True)
    assert {w.reason for w in ws} == {"slot_status_missing"} and not any(w.usable for w in ws)
    plain = parse.windows_from_manifest(lines)  # exploratory code path: unchanged
    assert any(w.usable for w in plain) and all(w.slot_status == "unknown" for w in plain)


def test_analyze_reports_skipped_slots_per_validator(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=12)
    wid = lines[0]["tx"]["window_id"]
    present = _present(lines, {wid: [lines[0]["tx"]["first_slot"] + 1]})
    res = analyze.analyze(lines, SIM_REF, present=present)
    (r,) = res["skipped_slots_by_validator"].values()
    assert r["windows_with_skipped_slot"] == 1 and res["skipped_slots_included"] is False


def test_merge_spans_and_record(tmp_path):
    assert blocklist.merge_spans([(10, 13), (20, 23), (5000, 5003)], max_gap=100) == [(10, 23), (5000, 5003)]
    assert blocklist.merge_spans([(0, 60_000), (60_010, 120_000)], max_gap=100, max_span=100_000) == [
        (0, 60_000), (60_010, 120_000)]
    windows = {"a": (100, 4), "b": (108, 4)}
    calls = []

    def rpc(method, params):
        calls.append(method)
        if method == "getSlot":
            return 1000
        return [100, 101, 103, 108, 109, 110, 111]  # slot 102 skipped

    rec = blocklist.build_record("live-x", windows, rpc)
    assert calls == ["getSlot", "getBlocks"]  # one merged range
    assert rec["windows"]["a"]["slots_with_blocks"] == [100, 101, 103]
    assert rec["windows"]["b"]["slots_with_blocks"] == [108, 109, 110, 111]
    assert rec["commitment"] == "finalized" and rec["calls"] == [{"start": 100, "end": 111, "n_blocks": 7}]
    blocklist.save(tmp_path, rec)
    assert blocklist.present_map(tmp_path, ["live-x", "live-none"]) == {"a": {100, 101, 103}, "b": {108, 109, 110, 111}}


def test_unfinalized_windows_and_bad_replies_are_refused():
    with pytest.raises(RuntimeError, match="not finalized"):
        blocklist.build_record("r", {"a": (100, 4)}, lambda m, p: 50 if m == "getSlot" else [])
    with pytest.raises(RuntimeError, match="outside the range"):
        blocklist.build_record("r", {"a": (100, 4)}, lambda m, p: 1000 if m == "getSlot" else [99])


def test_fetch_skips_existing_lists_and_delivery_check_windows(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=3)
    lines += synth(tmp_path / "x", sim.AGAVE, windows=2, seed=1, ordering=False)
    for l in lines[-10:]:
        l["tx"]["run_id"] = "live-delivery"
    seen = []

    def rpc(method, params):
        seen.append(method)
        return 10 ** 9 if method == "getSlot" else []

    out = blocklist.fetch(lines, tmp_path, rpc, log=lambda m: None)
    assert [p.parent.name for p in out] == ["live-20261101T000000Z"]  # delivery-check windows need no list
    n = len(seen)
    assert blocklist.fetch(lines, tmp_path, rpc, log=lambda m: None) == [] and len(seen) == n
    assert json.loads(out[0].read_text())["windows"]


def test_redaction():
    assert "SECRET" not in blocklist.redact("https://x/?api-key=SECRET&a=1")
