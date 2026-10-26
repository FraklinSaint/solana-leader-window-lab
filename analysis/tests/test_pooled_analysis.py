"""PREREGISTRATION §8 option B end to end on SYNTHETIC manifests: the pooled verdict is stratified
by independence unit and per-validator numbers stay descriptive."""
import copy

from slwlab import analyze, sim

from test_parse_analyze import SIM_REF, synth


def _second_validator(lines, identity):
    out = copy.deepcopy(lines)
    for l in out:
        l["tx"]["identity"] = identity
        l["tx"]["window_id"] += "-B"
        l["tx"]["sig"] = "B" + l["tx"]["sig"][1:]
    return out


def _lines(tmp_path, sched_a, sched_b, n_a=40, n_b=10):
    a = synth(tmp_path, sched_a, windows=n_a, seed=11)
    b = _second_validator(synth(tmp_path / "b", sched_b, windows=n_b, seed=12), "Va1idatorIdentity2222222222222222222222222")
    return a + b


def test_pooled_both_units_follow_the_rule(tmp_path):
    res = analyze.analyze_pooled(_lines(tmp_path, sim.AGAVE, sim.AGAVE), SIM_REF, ref_reps=10)
    (cell,) = res["cells"]
    assert len(cell["units_in_test"]) == 2 and res["min_unit_windows"] == 5
    r = cell["rules"]["R_RATIO_AGAVE"]
    assert r["p_holm"] < 0.01 and r["verdict"] in ("STRICT_RULE", "STRICT_RULE_LOW_SEPARATION", "SOFT_RULE")
    assert all(v["label"] == "DESCRIPTIVE ONLY" for v in cell["by_validator_descriptive"].values())
    assert not any("p_one_sided" in v for v in cell["by_validator_descriptive"].values())


def test_dominant_unit_does_not_carry_the_pooled_verdict(tmp_path):
    # 40 windows follow the rule, the other unit (10 windows) is random: the window-weighted sum
    # would be dominated by the first unit; the stratified mean averages the two units equally.
    lines = _lines(tmp_path, sim.AGAVE, sim.RANDOM)
    pooled = analyze.analyze_pooled(lines, SIM_REF, ref_reps=10)
    (cell,) = pooled["cells"]
    r = cell["rules"]["R_RATIO_AGAVE"]
    only_a = analyze.analyze(lines, SIM_REF)
    by_id = {c["identity"]: c for c in only_a["cells"]}
    assert by_id["Va1idatorIdentity1111111111111111111111111"]["rules"]["R_RATIO_AGAVE"]["p_holm"] < 0.01
    assert r["mean_tau_stratified"] < by_id["Va1idatorIdentity1111111111111111111111111"]["rules"]["R_RATIO_AGAVE"]["mean_tau"]


def test_one_big_unit_is_not_a_pool(tmp_path):
    lines = _lines(tmp_path, sim.AGAVE, sim.AGAVE, n_a=30, n_b=3)  # unit B below the 5-window minimum
    (cell,) = analyze.analyze_pooled(lines, SIM_REF, ref_reps=10)["cells"]
    assert cell["note"].startswith("fewer than 2 units")
    assert {v["verdict"] for v in cell["rules"].values()} == {"INCONCLUSIVE"}
    assert len(cell["by_validator_descriptive"]) == 2  # the small unit is still reported
