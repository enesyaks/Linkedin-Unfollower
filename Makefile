.PHONY: test test-rust test-js lint fmt build clean

test: test-rust test-js

test-rust:
	cd cli && cargo test

test-js:
	node web/test/unfollow.test.mjs

lint:
	cd cli && cargo fmt --check && cargo clippy --all-targets -- -D warnings

fmt:
	cd cli && cargo fmt

build:
	cd cli && cargo build --release
	@echo "binary: cli/target/release/liunfollow"

clean:
	cd cli && cargo clean
