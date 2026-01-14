set shell := ["zsh", "-lc"]

# Load environment variables from a local .env if you have one
set dotenv-load

default: test

build:
	cargo build

build-release:
	cargo build --release

test:
	cargo test -- --test-threads=1

test-integration:
	cargo test --test integration_test -- --test-threads=1

test-e2e:
	cargo test --test e2e_test -- --test-threads=1

# Run the 100MB E2E test only (requires restic CLI + valid 115 tokens)
test-e2e-100mb:
	cargo test --test e2e_test test_e2e_100mb -- --test-threads=1 --nocapture

