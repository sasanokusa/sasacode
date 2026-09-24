#!/bin/sh
# sasacode installer
#   curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh
#
# Environment:
#   SASACODE_VERSION       version to install, e.g. v0.4.0 (default: latest)
#   SASACODE_INSTALL_DIR   where to put the binary (default: ~/.local/bin)
#   SASACODE_DOWNLOAD_BASE download site (default: https://sasanokusa.com/sasacode)
set -eu

BASE="${SASACODE_DOWNLOAD_BASE:-https://sasanokusa.com/sasacode}"
INSTALL_DIR="${SASACODE_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${SASACODE_VERSION:-}"

say() { printf '%s\n' "$*"; }
die() { printf 'sasacode install: %s\n' "$*" >&2; exit 1; }

fetch() { # url [outfile]
  if command -v curl >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then curl -fsSL --retry 3 -o "$2" "$1"; else curl -fsSL --retry 3 "$1"; fi
  elif command -v wget >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then wget -q -O "$2" "$1"; else wget -q -O - "$1"; fi
  else
    die "curl or wget is required"
  fi
}

# ── platform ────────────────────────────────────────────────────────
case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  MINGW* | MSYS* | CYGWIN*) die "Windows is supported through WSL: run this inside a WSL shell" ;;
  *) die "unsupported OS: $(uname -s)" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch=x64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac

# An x64 shell under Rosetta on Apple Silicon should still get the native build.
if [ "$os" = darwin ] && [ "$arch" = x64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=arm64
fi

target="$os-$arch"
if [ "$os" = linux ]; then
  if [ -f /etc/alpine-release ] || ls /lib/ld-musl-* >/dev/null 2>&1; then
    target="$target-musl"
  elif [ "$arch" = x64 ] && ! grep -qw avx2 /proc/cpuinfo 2>/dev/null; then
    target="$target-baseline" # CPUs without AVX2
  fi
fi

# ── download ────────────────────────────────────────────────────────
if [ -z "$VERSION" ]; then
  VERSION="$(fetch "$BASE/latest" | tr -d '[:space:]')" || die "could not read $BASE/latest"
fi
[ -n "$VERSION" ] || die "could not determine the latest version"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
archive="sasacode-$target.tar.gz"
url="$BASE/releases/$VERSION"

say "Downloading sasacode $VERSION ($target)..."
fetch "$url/$archive" "$tmp/$archive" || die "download failed: $url/$archive"
fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $url/SHA256SUMS"

expected="$(grep " $archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$archive" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/sasacode-$target" "$INSTALL_DIR/sasacode"

"$INSTALL_DIR/sasacode" --version >/dev/null 2>&1 || die "installed binary does not run on this system"
say "Installed sasacode $("$INSTALL_DIR/sasacode" --version) to $INSTALL_DIR/sasacode"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "$INSTALL_DIR is not on your PATH. Add it, e.g.:"
    say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.$(basename "${SHELL:-sh}")rc"
    ;;
esac

say ""
say "Next: fill in an API key in ~/.sasacode/.env (blank entries are ignored), then run: sasacode"
