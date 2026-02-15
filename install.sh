#!/bin/sh
set -e

REPO="stevenselcuk/json-diff"
BINARY_NAME="json-diff"
INSTALL_DIR="/usr/local/bin"

# Detect OS and Arch
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

if [ "$ARCH" = "x86_64" ]; then
    ARCH="x86_64"
elif [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
    ARCH="aarch64"
else
    echo "Unsupported architecture: $ARCH"
    exit 1
fi

# Map macOS to 'apple-darwin' and Linux to 'unknown-linux-gnu' or similar
# Adjust these based on your actual release asset naming convention!
if [ "$OS" = "darwin" ]; then
    TARGET="apple-darwin"
elif [ "$OS" = "linux" ]; then
    TARGET="unknown-linux-gnu"
else
    echo "Unsupported OS: $OS"
    exit 1
fi

ASSET_NAME="${BINARY_NAME}-${ARCH}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET_NAME}"

echo "Downloading ${BINARY_NAME} for ${OS}/${ARCH}..."
echo "Url: ${DOWNLOAD_URL}"

# Create temp directory
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

# Download
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET_NAME"

# Extract
tar -xzf "$TMP_DIR/$ASSET_NAME" -C "$TMP_DIR"

# Install
echo "Installing to ${INSTALL_DIR} (requires sudo)..."
sudo mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"

echo "${BINARY_NAME} installed successfully!"
echo "Run it with: ${BINARY_NAME} --help"
