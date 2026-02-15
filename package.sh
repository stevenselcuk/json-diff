#!/bin/sh
set -e

# Configuration
BINARY_NAME="json-diff"
OUTPUT_DIR="target/release"

# Build optimized binary
echo "Building release binary..."
cargo build --release

# Detect OS and Arch
OS_RAW="$(uname -s)"
ARCH_RAW="$(uname -m)"

# Normalize Arch
if [ "$ARCH_RAW" = "x86_64" ]; then
    ARCH="x86_64"
elif [ "$ARCH_RAW" = "arm64" ] || [ "$ARCH_RAW" = "aarch64" ]; then
    ARCH="aarch64"
else
    echo "Warning: Unknown arch $ARCH_RAW"
    ARCH="$ARCH_RAW"
fi

# Normalize OS
if [ "$OS_RAW" = "Darwin" ]; then
    OS="apple-darwin"
elif [ "$OS_RAW" = "Linux" ]; then
    OS="unknown-linux-gnu"
else
    echo "Warning: Unknown OS $OS_RAW"
    OS="$OS_RAW"
fi

PACKAGE_NAME="${BINARY_NAME}-${ARCH}-${OS}.tar.gz"

echo "Packaging for ${OS}/${ARCH}..."

# Create tarball
tar -czf "$PACKAGE_NAME" -C "$OUTPUT_DIR" "$BINARY_NAME"

echo "---------------------------------------------------"
echo "Success! Package created: $PACKAGE_NAME"
echo ""
echo "Next Steps:"
echo "1. Go to https://github.com/stevenselcuk/json-diff/releases/new"
echo "2. Create a new release (e.g., v0.1.0)"
echo "3. Upload '$PACKAGE_NAME' as an asset."
echo ""
echo "Your users can then install it with:"
echo "curl -fsSL https://raw.githubusercontent.com/stevenselcuk/json-diff/main/install.sh | sh"
echo "---------------------------------------------------"
