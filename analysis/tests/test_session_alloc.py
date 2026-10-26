"""Per-session cumulative allocation files (PREREGISTRATION §8)."""
import json
import pathlib

from slwlab import session_alloc

ROOT = pathlib.Path(__file__).resolve().parents[1]
ALLOC = json.loads((ROOT / "sim" / "results" / "alloc_v2.json").read_text(encoding="utf-8"))


def _full():
    return session_alloc.build(ALLOC, "calibrated", 4)


def test_cumulative_quotas_are_monotone_and_the_last_is_the_full_quota():
    files = _full()
    assert [f["session"] for f in files] == [1, 2, 3, 4]
    full = ALLOC["scenarios"]["calibrated"]["allocation"]["plan_quota"]
    assert files[-1]["allocation"]["plan_quota"] == {s: a for s, a in sorted(full.items()) if s != "BamFiredancer"}
    for prev, cur in zip(files, files[1:]):
        for s, arms in cur["allocation"]["plan_quota"].items():
            for a, q in arms.items():
                assert q >= prev["allocation"]["plan_quota"][s][a]
    q1 = files[0]["allocation"]["plan_quota"]["Agave"]
    q4 = files[3]["allocation"]["plan_quota"]["Agave"]
    assert q1 == {a: -(-q // 4) for a, q in q4.items()}  # ceil(quota / 4)


def test_bamfiredancer_is_dropped_and_the_sender_can_read_the_file():
    for f in _full():
        assert "BamFiredancer" not in f["allocation"]["plan_quota"]
        pq = f["allocation"]["plan_quota"]
        # crates/sender/src/main.rs load_quotas: allocation.plan_quota -> stratum -> arm letter -> integer
        assert pq and all(set(arms) <= {"P", "X", "R", "C", "F"} and all(isinstance(n, int) and n > 0 for n in arms.values())
                          for arms in pq.values())


def test_carry_over_by_the_planners_rule():
    """The planner plans quota - done per validator and arm (prior_plan_state counts earlier sessions)."""
    files = _full()
    done = {a: 0 for a in files[0]["allocation"]["plan_quota"]["Agave"]}
    for k, f in enumerate(files):
        remaining = {a: f["allocation"]["plan_quota"]["Agave"][a] - done[a] for a in done}
        assert all(r >= 0 for r in remaining.values())
        # session 2 falls short by 5 windows in every arm: session 3 asks for them again
        take = {a: max(r - (5 if k == 1 else 0), 0) for a, r in remaining.items()}
        for a in done:
            done[a] += take[a]
        if k == 2:
            assert all(remaining[a] >= 5 for a in done)  # the shortfall of session 2 came back
    assert done == files[-1]["allocation"]["plan_quota"]["Agave"]


def test_files_are_deterministic_and_match_the_ones_on_disk(tmp_path):
    a, b = session_alloc.dump(_full()[0]), session_alloc.dump(_full()[0])
    assert a == b and b.endswith(b"\n") and b"\r" not in a
    assert session_alloc.main(["--out-dir", str(tmp_path)]) == 0
    assert session_alloc.main(["--out-dir", str(tmp_path), "--check"]) == 0
    (tmp_path / "alloc_calibrated_session1of4.json").write_bytes(b"{}\n")
    assert session_alloc.main(["--out-dir", str(tmp_path), "--check"]) == 1
    committed = ROOT / "sim" / "results" / "sessions"
    if committed.exists():
        assert session_alloc.main(["--out-dir", str(committed), "--check"]) == 0
