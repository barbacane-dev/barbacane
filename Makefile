.PHONY: all test test-verbose clippy fmt check build release plugins clean help

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

# Build all plugins (requires wasm32-wasip1 target)
plugins:
	@echo "Building plugins..."
	@for plugin in plugins/*/; do \
		if [ -f "$$plugin/Cargo.toml" ]; then \
			plugin_name=$$(basename "$$plugin"); \
			echo "Building $$plugin_name..."; \
			(cd "$$plugin" && cargo build --release --target wasm32-wasip1); \
			wasm_file=$$(ls "$$plugin/target/wasm32-wasip1/release/"*.wasm 2>/dev/null | grep -v deps | head -1); \
			if [ -n "$$wasm_file" ]; then \
				cp "$$wasm_file" "$$plugin/$$plugin_name.wasm"; \
				echo "  -> $$plugin_name.wasm"; \
			fi \
		fi \
	done
	@echo "Done building plugins"

# Clean build artifacts
clean:
	cargo clean
	@for plugin in plugins/*/; do \
		if [ -f "$$plugin/Cargo.toml" ]; then \
			(cd "$$plugin" && cargo clean); \
		fi \
	done

# Show help
help:
	@echo "Barbacane Makefile targets:"
	@echo ""
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
	@echo "  make clean        - Clean all build artifacts"
	@echo "  make help         - Show this help"
