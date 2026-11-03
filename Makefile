PY ?= python
.PHONY: reproduce test rust-test py-test lint facts verify verify-online report report-check secrets-check sim history-scan sessions merge

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
report:            # docs/REPORT.md from committed files (MEASUREMENT / EXPLORATORY / SIMULATION)
	cd analysis && $(PY) -m slwlab.report
report-check:      # exit 1 if docs/REPORT.md is stale or was edited by hand
	cd analysis && $(PY) -m slwlab.report --check
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
sessions:          # PLANNING: hours per validator/cell + time-constrained session plan -> docs/SESSIONS.md
	cd analysis && $(PY) -m slwlab.sessions
merge:             # EXPLORATORY: merge session chunks since SINCE (e.g. 20261024T000000Z) -> coverage by UTC hour
	cd analysis && $(PY) -m slwlab.merge_dryrun --since $(SINCE)
reproduce:         # verify the data offline, rerun the registered analysis, compare with analysis/results/ (~20 min)
	$(PY) scripts/reproduce.py
