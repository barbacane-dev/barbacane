.PHONY: all test test-verbose clippy fmt check build release plugins clean help \
        control-plane ui dev db-up db-down db-reset seed-plugins

# Default target
all: check test

# Run all tests
test:
	cargo test --workspace

# Run tests with output
test-verbose:
	cargo test --workspace -- --nocapture

# Run a specific test (usage: make test-one TEST=test_name)
test-one:
	cargo test --workspace $(TEST) -- --nocapture

# Run clippy lints
clippy:
	cargo clippy --workspace

# Format code
fmt:
	cargo fmt --all

# Check formatting without modifying
fmt-check:
	cargo fmt --all -- --check

# Full check (fmt + clippy + test)
check: fmt-check clippy

# Build debug
build:
	cargo build --workspace

# Build release
release:
	cargo build --workspace --release

# Build all plugins (requires wasm32-unknown-unknown target)
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

# Seed the plugin registry with built-in plugins
seed-plugins: plugins
	cargo run -p barbacane-control -- seed-plugins --plugins-dir plugins --database-url $(DATABASE_URL) --verbose

# Clean build artifacts
clean:
	cargo clean
	@for plugin in plugins/*/; do \
		if [ -f "$$plugin/Cargo.toml" ]; then \
			(cd "$$plugin" && cargo clean); \
		fi \
	done

# =============================================================================
# Development servers
# =============================================================================

# Default database URL for local development
DATABASE_URL ?= postgres://barbacane:barbacane@localhost:5432/barbacane

# Start the control plane server
control-plane:
	cargo run -p barbacane-control -- serve --database-url $(DATABASE_URL)

# Start the UI development server
ui:
	cd ui && npm run dev

# Start both control plane and UI (requires terminal multiplexer or run in separate terminals)
dev:
	@echo "Starting development environment..."
	@echo "Run 'make control-plane' in one terminal"
	@echo "Run 'make ui' in another terminal"
	@echo ""
	@echo "Or use: make dev-tmux (requires tmux)"

# Start dev environment with tmux
dev-tmux:
	@command -v tmux >/dev/null 2>&1 || { echo "tmux is required but not installed."; exit 1; }
	tmux new-session -d -s barbacane 'make control-plane' \; \
		split-window -h 'make ui' \; \
		attach

# =============================================================================
# Database commands
# =============================================================================

# Start the database (requires docker-compose)
db-up:
	docker-compose up -d postgres

# Stop the database
db-down:
	docker-compose down

# Reset the database (stop, remove volume, start fresh)
db-reset:
	docker-compose down -v
	docker-compose up -d postgres
	@echo "Waiting for database to be ready..."
	@sleep 3
	@echo "Database reset complete"

# Show help
help:
	@echo "Barbacane Makefile targets:"
	@echo ""
	@echo "Build & Test:"
	@echo "  make              - Run check + test (default)"
	@echo "  make test         - Run all workspace tests"
	@echo "  make test-verbose - Run tests with output"
	@echo "  make test-one TEST=name - Run specific test"
	@echo "  make clippy       - Run clippy lints"
	@echo "  make fmt          - Format all code"
	@echo "  make fmt-check    - Check formatting"
	@echo "  make check        - Run fmt-check + clippy"
	@echo "  make build        - Build debug"
	@echo "  make release      - Build release"
	@echo "  make plugins      - Build all WASM plugins"
	@echo "  make seed-plugins - Build plugins and seed registry"
	@echo "  make clean        - Clean all build artifacts"
	@echo ""
	@echo "Development:"
	@echo "  make control-plane - Start control plane server (port 9090)"
	@echo "  make ui            - Start UI dev server (port 5173)"
	@echo "  make dev           - Show instructions to start both"
	@echo "  make dev-tmux      - Start both in tmux session"
	@echo ""
	@echo "  Override DATABASE_URL: make control-plane DATABASE_URL=postgres://..."
	@echo ""
	@echo "Database:"
	@echo "  make db-up         - Start PostgreSQL container"
	@echo "  make db-down       - Stop PostgreSQL container"
	@echo "  make db-reset      - Reset database (removes all data)"
	@echo ""
	@echo "  make help          - Show this help"
