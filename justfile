fmt-check:
    cargo fmt --all -- --check

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --all-features --locked

lint: fmt-check clippy

fix-lint:
    cargo fmt --all
    cargo clippy --workspace --fix --allow-dirty --allow-staged --all-targets --all-features

test:
    cargo test --workspace --locked

# The worker binary dut-pool runs scripts in; must match the pinned Monty version.
install-worker:
    cargo install monty-runtime --version '=1.6.0' --locked
