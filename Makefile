.PHONY: test setup-lean test-lean test-texmacs

test:
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest evals/test_runner.py
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace

setup-lean:
	./scripts/setup-lean.sh

test-lean:
	test -x .toolchains/lean-4.19.0-darwin_aarch64/bin/lake
	test -f .mls/lake-manifest.json
	cp lean/MlsExtract.lean .mls/MlsExtract.lean
	cd .mls && PATH="$(CURDIR)/.toolchains/lean-4.19.0-darwin_aarch64/bin:$$PATH" MATHLIB_CACHE_DIR="$(CURDIR)/.toolchains/mathlib-cache" lake build MlsExtract
	PYTHONDONTWRITEBYTECODE=1 python3 tests/lean/test_repl.py
	MLS_LEAN_TESTS=1 cargo test -p mls-lean --test real_lean -- --nocapture
	MLS_LEAN_TESTS=1 cargo test -p mls-engine --test pipeline -- --nocapture
	MLS_LEAN_TESTS=1 cargo test -p mls-cli --test batch -- --nocapture
	MLS_LEAN_TESTS=1 cargo test -p mls-cli --test review -- --nocapture

test-texmacs:
	cargo build -p mls-cli
	sh tests/texmacs/run.sh
	MLS_TEXMACS_TESTS=1 cargo test -p mls-tm --test texmacs_roundtrip --test texmacs_oracle
