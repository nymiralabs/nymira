# `make demo`: a local stack with no keys or money (sandbox vault, operator, fake NEAR AI).
.PHONY: demo
demo:
	cargo run -p nymirad --features mock --example demo

# `make golden`: the whole first-release path in the mock stack, asserted step by step: deposit,
# lease, chat, settlement, close, treasury claim, leaf retirement (needs cargo-near 0.22.0).
.PHONY: golden
golden:
	cargo test -p nymirad --test golden_path --locked -- --nocapture

# What the quickstart CI job runs on a clean runner: the demo builds, and the golden path passes.
.PHONY: quickstart
quickstart:
	cargo build -p nymirad --features mock --example demo --locked
	$(MAKE) golden

# `make watch-test`: the vault key watcher against a stub node (needs Node 20 or newer).
.PHONY: watch-test
watch-test:
	node --test scripts/watch-vault-keys.test.mjs
