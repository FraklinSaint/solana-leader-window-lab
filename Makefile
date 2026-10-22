PY ?= python
.PHONY: test rust-test py-test lint facts verify verify-online secrets-check sim history-scan

test: rust-test py-test secrets-check
rust-test:
	cargo test --workspace --locked
py-test:
	cd analysis && $(PY) -m pytest
lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
facts:
	$(PY) scripts/verify_facts.py
verify:            # offline: manifest vs saved blocks, tx contents, exclusions, ledger, hidden data
	$(PY) scripts/verify_manifest.py
verify-online:     # also re-fetch every saved block, leaders and sig statuses from RPC_URL (read-only)
	$(PY) scripts/verify_manifest.py --online
secrets-check:
	$(PY) scripts/check_no_keys.py
sim:               # SIMULATION: ceilings, power, thresholds -> analysis/sim/results + docs/SIMULATION.md
	cd analysis && $(PY) -m slwlab.power --reps 200 --out sim/results/sim_v1.json
	cd analysis && $(PY) -m slwlab.allocate --reps 200 --out sim/results/alloc_v1.json --md sim/results/alloc_v1.md
	cd analysis && $(PY) -m slwlab.simreport --sim sim/results/sim_v1.json --out ../docs/SIMULATION.md
history-scan:      # before every push: whole git history + memory dir, never prints secrets
	$(PY) scripts/scan_history.py . $(MEMDIR)
