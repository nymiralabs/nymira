fmt:
	@cargo fmt

fmt-check:
	@cargo fmt --check

lint: fmt-check
	@cargo clippy --locked --all-targets -- -D warnings

test:
	@cargo test --locked

check: lint test
