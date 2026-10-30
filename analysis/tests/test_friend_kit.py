"""scripts/friend_kit_build.py and scripts/friend_data_check.py (AMENDMENT 3, platform B)."""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]


def _load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fk = _load("friend_kit_build")
cnk = _load("check_no_keys")


def test_allocation_is_generated_and_matches_the_committed_file():
    gen = fk.build()
    assert (fk.FILES / fk.ALLOC_NAME).read_bytes() == gen[fk.ALLOC_NAME]
    alloc = json.loads(gen[fk.ALLOC_NAME])
    plan = alloc["allocation"]["plan_quota"]
    assert all(q == {"C": 4, "F": 4, "P": 4, "X": 5} for q in plan.values())
    sel = json.loads(gen[fk.SELECTION_NAME])
    assert sorted(p["identity"][:8] for p in sel["picked"]) == sorted(fk.VALIDATORS)
    assert "candidates" not in sel and "raw" not in sel


def test_kit_members_hold_nothing_of_ours():
    m = fk.kit_members(None, None)
    assert sorted(m) == sorted(["allocation/alloc_platform_b.json", "data/snapshots/" + fk.SELECTION_NAME, "secrets/CLUSTER",
                                "README_FOR_OPERATOR_B.md", "COMMANDS_B.txt", "env.template"])
    blob = b"".join(m.values()).decode("utf-8", "replace")
    assert not any(x.group(1) != "REDACTED" for x in cnk.API_KEY.finditer(blob))
    assert not cnk.has_keypair_b58(blob)
    assert "RPC_URL=\n" in m["env.template"].decode()


def test_data_check_rejects_a_planted_keypair(tmp_path):
    seed = os.urandom(32)
    kp = list(seed + cnk.ed25519_public(seed))
    d = tmp_path / "data"
    d.mkdir()
    (d / "x.json").write_text(json.dumps(kp), encoding="utf-8")
    r = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "friend_data_check.py"), "--dir", str(d)], capture_output=True, text=True)
    assert r.returncode == 1 and "real keypair array" in r.stdout
