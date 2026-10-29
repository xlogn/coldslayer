#!/usr/bin/env sh
set -e

REPO="xlogn/coldslayer"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH="amd64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

if [ -d "/opt/homebrew/bin" ] && [ -w "/opt/homebrew/bin" ]; then
  INSTALL_DIR="/opt/homebrew/bin"
fi

echo "==> Installing csl (coldslayer) ($OS/$ARCH)..."

LATEST_TAG="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)"
VERSION="${LATEST_TAG#v}"

if [ -n "$VERSION" ]; then
  URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/coldslayer_${VERSION}_${OS}_${ARCH}.tar.gz"
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$TMP_DIR"' EXIT
  if curl -fsSL "$URL" -o "$TMP_DIR/coldslayer.tar.gz" 2>/dev/null; then
    tar -xzf "$TMP_DIR/coldslayer.tar.gz" -C "$TMP_DIR"
    mv "$TMP_DIR/csl" "$INSTALL_DIR/csl"
    chmod +x "$INSTALL_DIR/csl"
    ln -sf "$INSTALL_DIR/csl" "$INSTALL_DIR/coldslayer"
    echo "==> Installed csl to $INSTALL_DIR/csl (and alias $INSTALL_DIR/coldslayer)"
    "$INSTALL_DIR/csl" init
    exit 0
  fi
fi

if command -v cargo >/dev/null 2>&1; then
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$TMP_DIR"' EXIT
  git clone --depth 1 "https://github.com/${REPO}.git" "$TMP_DIR/coldslayer"
  cd "$TMP_DIR/coldslayer"
  cargo build --release
  cp target/release/csl "$INSTALL_DIR/csl"
  chmod +x "$INSTALL_DIR/csl"
  ln -sf "$INSTALL_DIR/csl" "$INSTALL_DIR/coldslayer"
  echo "==> Built and installed csl to $INSTALL_DIR/csl"
  "$INSTALL_DIR/csl" init
else
  echo "Error: Could not download prebuilt release and cargo is not installed." >&2
  exit 1
fi
