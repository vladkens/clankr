.PHONY: prepare check test test-cli build update clean run

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

test-cli:
	cargo build $(CARGO_FLAGS)
	uv run tests/cli.py --bin target/release/clankr $(if $(CASE),--case $(CASE))

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	cargo upgrade -i

clean:
	cargo clean

run:
	cargo build -r && clear && ./target/release/clankr
