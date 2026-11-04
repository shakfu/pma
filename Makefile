# pma

.DEFAULT_GOAL := build

.PHONY: build
build: ## compile a debug binary
	@cargo build

.PHONY: release
release: ## compile an optimised binary
	@cargo build --release

PREFIX ?= $(HOME)/.local

.PHONY: install
install: release ## install the release binary into ~/.local/bin (override PREFIX)
	@install -d $(PREFIX)/bin
	@install -m 755 target/release/pma $(PREFIX)/bin/pma
	@echo "installed $(PREFIX)/bin/pma"

.PHONY: test
test: ## run every test
	@cargo test
	@uv run --no-project --with pytest pytest -q scripts

.PHONY: herdr-check
herdr-check: ## run a dispatch through a real herdr server against a fresh fixture
	@sh scripts/herdr-check.sh

.PHONY: web-check
web-check: ## drive pma web in headless Chromium against a fresh fixture
	@sh scripts/web-check.sh

.PHONY: box-live
box-live: ## run pma-box against Docker and the network: engine, relay over TLS, whole runs
	@cargo test -p pma-box --test container_live -- --ignored --test-threads 1
	@cargo test -p pma-box --test live_tls -- --ignored
	@cargo test -p pma-box --test live_run -- --ignored --test-threads 1

.PHONY: check
check: ## format check, clippy with warnings as errors, and test
	@cargo fmt --check
	@cargo clippy --all-targets -- -D warnings
	@$(MAKE) --no-print-directory test

.PHONY: fmt
fmt: ## format the code
	@cargo fmt

.PHONY: help
help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-10s %s\n", $$1, $$2}'
