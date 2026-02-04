# =============================================================================
# Barbacane Makefile
# =============================================================================
#
# Quick reference:
#   make              Run checks and tests (default)
#   make dev-tmux     Start control plane + UI in tmux
#   make help         Show all targets
#
# =============================================================================

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

DATABASE_URL ?= postgres://barbacane:barbacane@localhost:5432/barbacane

# -----------------------------------------------------------------------------
# Default
# -----------------------------------------------------------------------------

.PHONY: all
all: check test

# -----------------------------------------------------------------------------
# Build
# -----------------------------------------------------------------------------

.PHONY: build release plugins seed-plugins clean

build:
	cargo build --workspace

release:
	cargo build --workspace --release

plugins:
	@echo "Building plugins..."
	@for plugin in plugins/*/; do \
		if [ -f "$$plugin/Cargo.toml" ]; then \
			plugin_name=$$(basename "$$plugin"); \
			echo "Building $$plugin_name..."; \
			(cd "$$plugin" && cargo build --release --target wasm32-unknown-unknown); \
			wasm_file=$$(ls "$$plugin/target/wasm32-unknown-unknown/release/"*.wasm 2>/dev/null | grep -v deps | head -1); \
			if [ -n "$$wasm_file" ]; then \
				cp "$$wasm_file" "$$plugin/$$plugin_name.wasm"; \
				echo "  -> $$plugin_name.wasm"; \
			fi \
		fi \
	done
	@echo "Done building plugins"

seed-plugins: plugins
	cargo run -p barbacane-control -- seed-plugins --plugins-dir plugins --database-url $(DATABASE_URL) --verbose

clean:
	cargo clean
	@for plugin in plugins/*/; do \
		if [ -f "$$plugin/Cargo.toml" ]; then \
			(cd "$$plugin" && cargo clean); \
		fi \
	done

# -----------------------------------------------------------------------------
# Test & Lint
# -----------------------------------------------------------------------------

.PHONY: test test-verbose test-one check clippy fmt fmt-check

test:
	cargo test --workspace

test-verbose:
	cargo test --workspace -- --nocapture

test-one:
	cargo test --workspace $(TEST) -- --nocapture

check: fmt-check clippy

clippy:
	cargo clippy --workspace

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

# -----------------------------------------------------------------------------
# Development (native)
# -----------------------------------------------------------------------------

.PHONY: control-plane ui dev dev-tmux

control-plane:
	cargo run -p barbacane-control -- serve --database-url $(DATABASE_URL)

ui:
	cd ui && npm run dev

dev:
	@echo "Run in separate terminals:"
	@echo "  make db-up          # Start PostgreSQL"
	@echo "  make control-plane  # Start API (port 9090)"
	@echo "  make ui             # Start UI (port 5173)"
	@echo ""
	@echo "Or use: make dev-tmux"

dev-tmux:
	@command -v tmux >/dev/null 2>&1 || { echo "tmux is required but not installed."; exit 1; }
	tmux new-session -d -s barbacane 'make control-plane' \; \
		split-window -h 'make ui' \; \
		attach

# -----------------------------------------------------------------------------
# Database
# -----------------------------------------------------------------------------

.PHONY: db-up db-down db-reset

db-up:
	docker-compose up -d postgres

db-down:
	docker-compose down

db-reset:
	docker-compose down -v
	docker-compose up -d postgres
	@echo "Waiting for database to be ready..."
	@sleep 3
	@echo "Database reset complete"

# -----------------------------------------------------------------------------
# Docker
# -----------------------------------------------------------------------------

.PHONY: docker-build docker-build-gateway docker-build-control \
        docker-up docker-down docker-run docker-run-control

docker-build: docker-build-gateway docker-build-control

docker-build-gateway:
	docker build -t barbacane .

docker-build-control:
	docker build -f Dockerfile.control -t barbacane-control .

docker-up:
	docker-compose up -d

docker-down:
	docker-compose down

docker-run:
	docker run -p 8080:8080 -v ./artifact.bca:/config/api.bca barbacane

docker-run-control:
	docker-compose up control-plane

# -----------------------------------------------------------------------------
# Help
# -----------------------------------------------------------------------------

.PHONY: help
help:
	@echo "Barbacane Makefile"
	@echo ""
	@echo "Build & Test:"
	@echo "  make                Run check + test (default)"
	@echo "  make build          Build debug"
	@echo "  make release        Build release"
	@echo "  make test           Run all tests"
	@echo "  make test-verbose   Run tests with output"
	@echo "  make test-one TEST=name"
	@echo "  make check          Run fmt-check + clippy"
	@echo "  make clippy         Run clippy lints"
	@echo "  make fmt            Format code"
	@echo "  make plugins        Build WASM plugins"
	@echo "  make seed-plugins   Build and seed plugin registry"
	@echo "  make clean          Clean build artifacts"
	@echo ""
	@echo "Development:"
	@echo "  make control-plane  Start API server (port 9090)"
	@echo "  make ui             Start UI dev server (port 5173)"
	@echo "  make dev            Show dev instructions"
	@echo "  make dev-tmux       Start both in tmux"
	@echo ""
	@echo "Database:"
	@echo "  make db-up          Start PostgreSQL"
	@echo "  make db-down        Stop PostgreSQL"
	@echo "  make db-reset       Reset database"
	@echo ""
	@echo "Docker:"
	@echo "  make docker-build          Build all images"
	@echo "  make docker-build-gateway  Build data plane image"
	@echo "  make docker-build-control  Build control plane image"
	@echo "  make docker-up             Start full stack (compose)"
	@echo "  make docker-down           Stop full stack"
	@echo "  make docker-run            Run data plane standalone"
	@echo "  make docker-run-control    Run control plane only"
	@echo ""
	@echo "Override DATABASE_URL:"
	@echo "  make control-plane DATABASE_URL=postgres://..."
