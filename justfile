# First-time setup: install git hooks
setup:
    git config core.hooksPath .githooks
    @echo "Git hooks installed (.githooks/pre-commit)"

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Web dashboard
build-web:
    cd web && npm ci && npm run build

# Full release build with embedded UI
build-release: build-web
    cargo build --workspace --release --features genesis-gateway/embed-ui

# Development
dev:
    @echo "Start in two terminals:"
    @echo "  Terminal 1: cargo run -- serve"
    @echo "  Terminal 2: cd web && npm run dev"

# Lint everything
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace -- -D warnings
    cd web && npm run lint

# Test everything
test:
    cargo test --workspace
    cd web && npm run build
