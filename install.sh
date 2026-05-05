#!/bin/sh
set -e

REPO="Alfredvc/cct"
BINARY="cct"
INSTALL_DIR="${CCT_INSTALL_DIR:-${HOME}/.local/bin}"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
  linux)  OS="linux" ;;
  darwin) OS="darwin" ;;
  *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

if [ -z "${CCT_VERSION:-}" ]; then
  CCT_VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
  if [ -z "$CCT_VERSION" ]; then
    echo "Error: could not determine latest release version" >&2
    exit 1
  fi
fi

VERSION="${CCT_VERSION#v}"
STAGE="cct-${VERSION}-${OS}-${ARCH}"
URL="https://github.com/${REPO}/releases/download/${CCT_VERSION}/${STAGE}.tar.gz"

mkdir -p "$INSTALL_DIR"

echo "Downloading ${BINARY} ${CCT_VERSION} for ${OS}/${ARCH}..."
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fsSL "$URL" | tar xz -C "$TMP"
mv "$TMP/$STAGE/$BINARY" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"
echo "Installed ${BINARY} to ${INSTALL_DIR}/${BINARY}"

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *) echo ""
     echo "Note: ${INSTALL_DIR} is not in your PATH. Add it with:"
     echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
     ;;
esac

echo "Run '${BINARY} --help' to get started."
