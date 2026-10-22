PY ?= python
.PHONY: test rust-test py-test lint facts verify secrets-check

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
verify:            # Phase 1: replaced by manifest/block verification
	$(PY) scripts/verify_facts.py
secrets-check:
	$(PY) scripts/check_no_keys.py
