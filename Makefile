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

.PHONY: test-cli run dist

test-cli: # the terminal scenarios of tests/cli.py on the release binary; CASE=<case> runs one
	cargo build $(CARGO_FLAGS)
	uv run tests/cli.py --bin target/release/clankr $(if $(CASE),--case $(CASE))

run: # build and start the CLI on a cleared screen
	cargo build -r && clear && ./target/release/clankr

# Prebuilt binaries as target/distrib/clankr-dev-<target>.tar.gz (.zip for Windows), from an Apple
# Silicon Mac: Linux (glibc 2.28) and Windows through cargo-cross, installed at a fixed version,
# with its toolchains kept in ~/.cache/cargo-cross (CI caches that folder).
# macOS keeps its file and line info in clankr.dSYM, which has to stay next to the binary. Not
# target/dist: cargo keeps the dist profile's build scripts there.
dist: T := $(or $(CARGO_TARGET_DIR),target)
dist: # the release archives for CI (vladkens/actions/rust-build)
	cargo install --locked cargo-cross@1.6.0
	CARGO_PROFILE_DIST_SPLIT_DEBUGINFO=packed cargo build --profile dist --locked --target aarch64-apple-darwin
	CROSS_COMPILER_DIR=$${CROSS_COMPILER_DIR:-$$HOME/.cache/cargo-cross} \
		cargo cross build --profile dist --locked --glibc-version 2.28 \
		--targets x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu,x86_64-pc-windows-gnu
	rm -rf $(T)/distrib && mkdir -p $(T)/distrib
	tar czhf $(T)/distrib/clankr-dev-aarch64-apple-darwin.tar.gz readme.md LICENSE \
		-C $(T)/aarch64-apple-darwin/dist clankr clankr.dSYM
	for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do \
		tar czf $(T)/distrib/clankr-dev-$$t.tar.gz readme.md LICENSE -C $(T)/$$t/dist clankr || exit 1; done
	zip -jq $(T)/distrib/clankr-dev-x86_64-pc-windows-gnu.zip readme.md LICENSE \
		$(T)/x86_64-pc-windows-gnu/dist/clankr.exe
	ls -lh $(T)/distrib
