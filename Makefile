# `make demo`: a local stack with no keys or money (sandbox vault, operator, fake NEAR AI).
.PHONY: demo
demo:
	cargo run -p nymirad --features mock --example demo
