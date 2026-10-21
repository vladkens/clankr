.PHONY: prepare check test build update clean

CARGO_FLAGS := --release --locked

prepare:
	cargo +nightly fmt
	cargo clippy $(CARGO_FLAGS) --fix --all-targets --allow-dirty --allow-no-vcs -- -D warnings
	cargo check $(CARGO_FLAGS)

check:
	cargo +nightly fmt --check
	cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings
	cargo check $(CARGO_FLAGS)

test:
	cargo test $(CARGO_FLAGS)

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/clankr

update:
	cargo upgrade -i

clean:
	cargo clean

.PHONY: test-cli run

test-cli: # the terminal scenarios of tests/cli.py on the release binary; CASE=<case> runs one
	cargo build $(CARGO_FLAGS)
	uv run tests/cli.py --bin target/release/clankr $(if $(CASE),--case $(CASE))

run: # build and start the CLI on a cleared screen
	cargo build -r && clear && ./target/release/clankr
