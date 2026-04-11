#!/bin/sh
set -e

REPO="634750802/claude9"
INSTALL_DIR="${CLAUDE9_INSTALL_DIR:-/usr/local/bin}"

main() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) platform="darwin" ;;
        Linux)  platform="linux" ;;
        *)      echo "Unsupported OS: $os" >&2; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="amd64" ;;
        aarch64|arm64) arch="arm64" ;;
        *)             echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac

    binary="claude9-${platform}-${arch}"

    if [ -n "$1" ]; then
        tag="$1"
    else
        tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)"
    fi

    if [ -z "$tag" ]; then
        echo "Failed to determine latest release." >&2
        exit 1
    fi

    url="https://github.com/${REPO}/releases/download/${tag}/${binary}"

    tmpfile="$(mktemp)"
    echo "Downloading claude9 ${tag} (${platform}/${arch})..."
    curl -fSL --progress-bar -o "$tmpfile" "$url"
    chmod +x "$tmpfile"

    if [ "$platform" = "darwin" ]; then
        xattr -d com.apple.quarantine "$tmpfile" 2>/dev/null || true
    fi

    if [ -w "$INSTALL_DIR" ]; then
        mv "$tmpfile" "${INSTALL_DIR}/claude9"
    else
        echo "Installing to ${INSTALL_DIR} (requires sudo)..."
        sudo mv "$tmpfile" "${INSTALL_DIR}/claude9"
    fi

    echo "claude9 ${tag} installed to ${INSTALL_DIR}/claude9"
}

main "$@"
