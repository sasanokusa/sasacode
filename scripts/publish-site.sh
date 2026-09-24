#!/bin/sh
# Mirror a GitHub release to sasanokusa.com/sasacode and point "latest" at it.
#   scripts/publish-site.sh v0.4.0 [user@host]
# Layout on the server (Apache docroot, served through the Cloudflare tunnel):
#   /var/www/html/sasacode/{index.html,install.sh,latest,.htaccess}
#   /var/www/html/sasacode/releases/<version>/{sasacode-<target>.tar.gz,SHA256SUMS}
set -eu
VERSION="${1:?usage: publish-site.sh <version> [user@host]}"
HOST="${2:-sasa@100.65.215.48}"
ROOT=/var/www/html/sasacode
REPO=sasanokusa/sasacode
cd "$(dirname "$0")/.."

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
gh release download "$VERSION" --repo "$REPO" --dir "$tmp" --pattern '*.tar.gz' --pattern SHA256SUMS
(cd "$tmp" && shasum -a 256 -c SHA256SUMS)

ssh "$HOST" "mkdir -p $ROOT/releases/$VERSION"
scp -q "$tmp"/*.tar.gz "$tmp"/SHA256SUMS "$HOST:$ROOT/releases/$VERSION/"
scp -q scripts/install.sh site/index.html site/.htaccess "$HOST:$ROOT/"
# Switch "latest" only after every file of the release is in place.
ssh "$HOST" "printf '%s\n' '$VERSION' > $ROOT/latest.tmp && mv $ROOT/latest.tmp $ROOT/latest && ls -la $ROOT $ROOT/releases/$VERSION"
echo "published $VERSION → https://sasanokusa.com/sasacode/"
