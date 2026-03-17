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
    cargo clippy --workspace -- -D warnings
    cd web && npm run lint

# Test everything
test:
    cargo test --workspace
    cd web && npm run build
