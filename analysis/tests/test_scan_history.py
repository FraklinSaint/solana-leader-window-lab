"""scripts/scan_history.py: the ed25519 keypair test and the exit code on a scratch repo."""
import gzip
import importlib.util
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("scan_history", ROOT / "scripts" / "scan_history.py")
sh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sh)

SCRIPT = ROOT / "scripts" / "scan_history.py"


def test_rfc8032_vectors():
    s1 = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
    p1 = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    s2 = bytes.fromhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb")
    p2 = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"
    assert sh.ed25519_public(s1).hex() == p1
    assert sh.ed25519_public(s2).hex() == p2


def _keypair(n):
    seed = bytes((n * 7 + i) % 256 for i in range(32))
    return list(seed + sh.ed25519_public(seed))


def test_keypair_vs_lookalike_arrays():
    kp = _keypair(1)
    assert sh.is_real_keypair(kp)
    assert not sh.is_real_keypair(list(range(64)))
    assert not sh.is_real_keypair(kp[:32] + kp[:32])  # right shape, wrong public half
    assert not sh.is_real_keypair(kp[:63])
    assert not sh.is_real_keypair(kp[:63] + [256])


def test_base58():
    assert sh.b58(b"\0\0\x01") == "112"
    assert sh.b58(bytes([0xff])) == "5Q"


def _git(repo, *a):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", *a],
                   cwd=repo, check=True, capture_output=True)


def _run(repo):
    r = subprocess.run([sys.executable, "-I", str(SCRIPT), str(repo)], capture_output=True, text=True)
    return r.returncode, r.stdout


def _repo(tmp_path, files):
    _git(tmp_path, "init", "-q")
    for name, data in files.items():
        p = tmp_path / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    _git(tmp_path, "add", "-A")
    _git(tmp_path, "commit", "-q", "-m", "x")


def test_clean_with_non_keypair_array_in_gz(tmp_path):
    arr = json.dumps(list(range(64))).encode()
    _repo(tmp_path, {"data/raw/blocks/1.json.gz": gzip.compress(b'{"accounts":' + arr + b"}")})
    code, out = _run(tmp_path)
    assert code == 0, out
    assert "in history: 1 seen, 1 not keypairs" in out and "in history: 1 seen, 1 not keypairs (pubkey check), 0 real" in out


def test_real_keypair_fails_even_in_data(tmp_path):
    arr = json.dumps(_keypair(2)).encode()
    _repo(tmp_path, {"data/raw/blocks/1.json.gz": gzip.compress(arr)})
    code, out = _run(tmp_path)
    assert code == 1 and "in history: 1 seen, 0 not keypairs (pubkey check), 1 real" in out


def test_own_secret_key_in_any_form_fails(tmp_path):
    kp = bytes(_keypair(3))
    (tmp_path / "secrets").mkdir()
    (tmp_path / "secrets" / "k.json").write_text(json.dumps(list(kp)))
    (tmp_path / ".gitignore").write_text("/secrets/*\n")
    _repo(tmp_path, {"docs/a.md": ("seed " + sh.b58(kp[:32]) + "\n").encode()})
    code, out = _run(tmp_path)
    assert code == 1 and "own-key matches: 1" in out.split("working tree")[0]
    _git(tmp_path, "rm", "-q", "docs/a.md")
    _git(tmp_path, "commit", "-q", "-m", "rm")
    # still in history
    assert _run(tmp_path)[0] == 1
