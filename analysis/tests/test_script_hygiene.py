"""No tracked script may contain patterns that antivirus heuristics flag (LOGBOOK 2026-10-23:
a PowerShell runner using in-memory P/Invoke was removed from the operator's disk). The
Windows calls the lab needs live in the Rust crate `slwlab-winsys`; the entry point is the
`sender` binary itself (README "Running natively").

Scope: every git-tracked script-like file (PowerShell, cmd/bat, VBScript, JavaScript, shell,
Python, Makefile, Dockerfile, compose and CI files). A PowerShell or cmd wrapper, if one is
ever added, must be a single command line that only calls the sender binary.
"""

import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
SELF = pathlib.Path(__file__).resolve()

SCRIPT_SUFFIXES = {".ps1", ".psm1", ".psd1", ".cmd", ".bat", ".vbs", ".js", ".sh", ".py", ".yml", ".yaml"}
SCRIPT_NAMES = {"Makefile", "Dockerfile"}
WRAPPER_SUFFIXES = {".ps1", ".psm1", ".cmd", ".bat"}

# Built from parts so that this file does not match itself in other scanners.
FLAGGED = [
    ("Add" + "-Type", r"\badd" + r"-type\b"),
    ("Dll" + "Import", r"dll" + r"import"),
    ("InteropServices / P-Invoke", r"interop" + r"services|p/?" + r"invoke\b"),
    ("encoded command", r"-enc(oded)?(command)?\s+[a-z0-9+/=]{16,}|" + r"encoded" + r"command"),
    ("FromBase64String", r"frombase64" + r"string"),
    ("Invoke-Expression", r"invoke" + r"-expression|(^|[;|&(\s])iex(\s|\(|$)"),
    ("hidden window", r"window" + r"style\s+hidden|\s-w(indowstyle)?\s+hid" + r"den\b|createnowindow"),
]


def tracked() -> list[pathlib.Path]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return [ROOT / p for p in out.decode("utf-8").split("\0") if p]


def is_script(p: pathlib.Path) -> bool:
    return p.suffix.lower() in SCRIPT_SUFFIXES or p.name in SCRIPT_NAMES or "scripts" in p.relative_to(ROOT).parts


def violations(text: str) -> list[str]:
    low = text.lower()
    return [name for name, pat in FLAGGED if re.search(pat, low, re.MULTILINE)]


def test_no_flagged_patterns_in_tracked_scripts():
    hits = []
    for p in tracked():
        if p.resolve() == SELF or not p.is_file() or not is_script(p):
            continue
        try:
            text = p.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for name in violations(text):
            hits.append(f"{p.relative_to(ROOT)}: {name}")
    assert hits == [], hits


def test_wrappers_are_one_line_calls_of_the_binary():
    bad = []
    for p in tracked():
        if p.suffix.lower() not in WRAPPER_SUFFIXES:
            continue
        lines = [l.strip() for l in p.read_text(encoding="utf-8").splitlines()
                 if l.strip() and not l.strip().startswith(("#", "::", "rem ", "REM ", "@echo off"))]
        if len(lines) != 1 or "sender" not in lines[0]:
            bad.append(str(p.relative_to(ROOT)))
    assert bad == [], bad


def test_the_check_catches_each_pattern():
    samples = {
        "Add" + "-Type": "Add" + "-Type -Name X -MemberDefinition '...'",
        "Dll" + "Import": "[Dll" + 'Import("kernel32.dll")]',
        "InteropServices / P-Invoke": "[System.Runtime.Interop" + "Services.Marshal]",
        "encoded command": "powershell -enc " + "SQBFAFgAIAAoAE4AZQB3AC0ATwBi",
        "FromBase64String": "[Convert]::FromBase64" + "String($x)",
        "Invoke-Expression": "Invoke" + "-Expression $code",
        "hidden window": "Start-Process x -Window" + "Style Hid" + "den",
    }
    for name, line in samples.items():
        assert name in violations(line), (name, line)
    assert violations("& target\\release\\sender.exe session --mode dryrun --hours 8") == []
    assert violations("iex (New-Object Net.WebClient)") == ["Invoke-Expression"]
