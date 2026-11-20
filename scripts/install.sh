#!/bin/sh
# sasacode installer
#   curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh
#
# Binaries come from GitHub Releases, so a new release is picked up without touching the site.
# The download is checked against the release's SHA256SUMS and, when gh is installed and signed in,
# against the release's signed build provenance (gh attestation verify).
#
# Environment:
#   SASACODE_VERSION       version to install, e.g. v0.5.0 (default: the latest release)
#   SASACODE_INSTALL_DIR   where to put the binary (default: ~/.local/bin)
#   SASACODE_DOWNLOAD_BASE releases URL, https only, or http to localhost (default: https://github.com/sasanokusa/sasacode/releases)
set -eu

# Everything that runs is in main, called on the last line: a script that arrives only in part (a
# dropped connection under `curl | sh`) defines functions and runs nothing.

REPO_SLUG="sasanokusa/sasacode"
DEFAULT_BASE="https://github.com/$REPO_SLUG/releases"

say() { printf '%s\n' "$*"; }
die() { printf 'sasacode install: %s\n' "$*" >&2; exit 1; }

# https only (http to this machine is the one exception: a test server), TLS 1.2 or newer, also
# after a redirect. PROTO is what curl may speak; main widens it for a base on this machine.
PROTO='=https'

fetch() { # url [outfile]
  if command -v curl >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then
      curl -fsSL --proto "$PROTO" --proto-redir "$PROTO" --tlsv1.2 --retry 3 -o "$2" "$1"
    else
      curl -fsSL --proto "$PROTO" --proto-redir "$PROTO" --tlsv1.2 --retry 3 "$1"
    fi
  elif command -v wget >/dev/null 2>&1; then
    # BusyBox's wget has no --https-only: the check on the base (in main) stands in for it.
    if [ "$PROTO" = '=https' ] && wget --help 2>&1 | grep -q -- --https-only; then
      if [ $# -eq 2 ]; then wget -q --https-only -O "$2" "$1"; else wget -q --https-only -O - "$1"; fi
    else
      if [ $# -eq 2 ]; then wget -q -O "$2" "$1"; else wget -q -O - "$1"; fi
    fi
  else
    die "curl or wget is required"
  fi
}

# The checksum comes from the release itself; the signed build provenance says who built the file.
# It is checked when gh can check it (installed and signed in), for the official releases. A failed
# check stops the install; one that cannot be made says so, with how to make it by hand.
verify_attestation() { # file
  hint="gh attestation verify <the downloaded file> --repo $REPO_SLUG"
  if [ "$BASE" != "$DEFAULT_BASE" ]; then
    say "Build provenance was not checked (not the official releases). By hand: $hint"
    return 0
  fi
  if ! command -v gh >/dev/null 2>&1; then
    say "Build provenance was not checked (gh is not installed). By hand: $hint"
    return 0
  fi
  if ! gh auth status >/dev/null 2>&1; then
    say "Build provenance was not checked (gh is not signed in). By hand: $hint"
    return 0
  fi
  if ! checked="$(gh attestation verify "$1" --repo "$REPO_SLUG" 2>&1)"; then
    printf '%s\n' "$checked" >&2
    die "build provenance check failed for $(basename "$1"); nothing was installed"
  fi
  say "Build provenance verified."
}

main() {
  BASE="${SASACODE_DOWNLOAD_BASE:-$DEFAULT_BASE}"
  INSTALL_DIR="${SASACODE_INSTALL_DIR:-$HOME/.local/bin}"
  VERSION="${SASACODE_VERSION:-}"
  case "$BASE" in
    https://*) ;;
    http://localhost | http://localhost[:/]* | http://127.0.0.1 | http://127.0.0.1[:/]* | http://\[::1\] | http://\[::1\][:/]*)
      PROTO='=http,https' ;;
    *) die "SASACODE_DOWNLOAD_BASE must be an https:// URL (http is allowed only for localhost)" ;;
  esac

  # ── platform ──────────────────────────────────────────────────────
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) die "on Windows, install from PowerShell: irm https://sasanokusa.com/sasacode/install.ps1 | iex" ;;
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

  # ── download ──────────────────────────────────────────────────────
  # Pin the release once, so the archive and its checksum always come from the same one. The version
  # is read from the redirect of …/latest/download/ itself (…/download/<version>/…): the
  # …/releases/latest page can lag behind it for a while after a release.
  if [ -z "$VERSION" ] && command -v curl >/dev/null 2>&1; then
    VERSION="$(curl -fsSI --proto "$PROTO" --tlsv1.2 "$BASE/latest/download/SHA256SUMS" 2>/dev/null | tr -d '\r' |
      sed -n 's|^[Ll]ocation: .*/download/\([^/]*\)/SHA256SUMS.*|\1|p' | head -n 1)" || VERSION=""
  fi
  if [ -n "$VERSION" ]; then url="$BASE/download/$VERSION"; else url="$BASE/latest/download"; fi

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT INT TERM
  archive="sasacode-$target.tar.gz"

  say "Downloading sasacode ${VERSION:-latest} ($target)..."
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
  verify_attestation "$tmp/$archive"

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
}

main "$@"
