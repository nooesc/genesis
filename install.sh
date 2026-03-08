#!/usr/bin/env bash
set -euo pipefail

# Genesis installer
# Installs the genesis CLI by building from source.
#
# Usage:
#   curl -sSf https://raw.githubusercontent.com/nooesc/genesis/main/install.sh | bash
#   # or with options:
#   curl -sSf ... | bash -s -- --backend openrouter --model anthropic/claude-sonnet-4-6

GENESIS_REPO="https://github.com/nooesc/genesis.git"
INSTALL_DIR="${GENESIS_INSTALL_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { echo -e "${BLUE}[info]${NC} $1"; }
success() { echo -e "${GREEN}[ok]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; }
error() { echo -e "${RED}[error]${NC} $1" >&2; }

# --- Dependency checks ---

check_dependency() {
    if ! command -v "$1" &> /dev/null; then
        error "$1 is required but not installed."
        echo "  Install it with: $2"
        return 1
    fi
}

info "Checking dependencies..."

if ! check_dependency "rustc" "https://rustup.rs"; then
    error "Rust is required to build Genesis from source."
    echo ""
    echo "  Install Rust:"
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo ""
    exit 1
fi

check_dependency "cargo" "https://rustup.rs" || exit 1
check_dependency "git" "your package manager (apt install git, brew install git)" || exit 1

RUST_VERSION=$(rustc --version | awk '{print $2}')
success "Rust $RUST_VERSION"

# --- Clone or update ---

CLONE_DIR="${GENESIS_CLONE_DIR:-${TMPDIR:-/tmp}/genesis-install-$$}"

cleanup() {
    if [ -d "$CLONE_DIR" ] && [ -z "${GENESIS_CLONE_DIR:-}" ]; then
        rm -rf "$CLONE_DIR"
    fi
}
trap cleanup EXIT

info "Cloning Genesis..."
git clone --depth 1 "$GENESIS_REPO" "$CLONE_DIR" 2>/dev/null
success "Cloned to $CLONE_DIR"

# --- Build ---

info "Building Genesis (release mode)..."
cd "$CLONE_DIR"
cargo build --release --quiet 2>&1

BINARY="$CLONE_DIR/target/release/genesis"
if [ ! -f "$BINARY" ]; then
    error "Build failed — binary not found at $BINARY"
    exit 1
fi

success "Built successfully"

# --- Install ---

mkdir -p "$INSTALL_DIR"
cp "$BINARY" "$INSTALL_DIR/genesis"
chmod +x "$INSTALL_DIR/genesis"
success "Installed to $INSTALL_DIR/genesis"

# Check if install dir is in PATH
if ! echo "$PATH" | tr ':' '\n' | grep -q "^$INSTALL_DIR$"; then
    warn "$INSTALL_DIR is not in your PATH"
    echo ""
    echo "  Add it to your shell profile:"
    echo "    echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc"
    echo "    # or for zsh:"
    echo "    echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc"
    echo ""
fi

# --- Initialize ---

info "Initializing Genesis..."

INIT_ARGS=""
# Pass through any CLI arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --backend)
            INIT_ARGS="$INIT_ARGS --backend $2"
            shift 2
            ;;
        --model)
            INIT_ARGS="$INIT_ARGS --model $2"
            shift 2
            ;;
        --base-url)
            INIT_ARGS="$INIT_ARGS --base-url $2"
            shift 2
            ;;
        --api-key-env)
            INIT_ARGS="$INIT_ARGS --api-key-env $2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

"$INSTALL_DIR/genesis" init $INIT_ARGS

echo ""
echo -e "${GREEN}Genesis is installed!${NC}"
echo ""
echo "  Quick start:"
echo "    genesis chat              # Start talking to Eve"
echo "    genesis doctor            # Check system health"
echo "    genesis info              # Show system overview"
echo "    genesis --help            # See all commands"
echo ""
