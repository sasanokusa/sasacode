#!/bin/sh
# Deploy the sasanokusa.com/sasacode landing page. Releases themselves are served from GitHub:
# /sasacode/install.sh redirects to the installer attached to the latest release.
#   scripts/publish-site.sh [user@host]
# The host is the first argument, else $SASACODE_SITE_HOST. {{VERSION}} in the HTML is replaced
# with the version in packages/cli/package.json (e.g. v0.9.7) in a temporary copy at deploy time.
set -eu
HOST="${1:-${SASACODE_SITE_HOST:-}}"
if [ -z "$HOST" ]; then
  echo "usage: scripts/publish-site.sh <[user@]host>   (or set SASACODE_SITE_HOST)" >&2
  exit 1
fi
ROOT=/var/www/html/sasacode
cd "$(dirname "$0")/.."
VERSION="v$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' packages/cli/package.json | head -n 1)"
if [ "$VERSION" = "v" ]; then
  echo "could not read the version from packages/cli/package.json" >&2
  exit 1
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/docs"
sed "s/{{VERSION}}/$VERSION/g" site/index.html > "$TMP/index.html"
sed "s/{{VERSION}}/$VERSION/g" site/docs/index.html > "$TMP/docs/index.html"
ssh "$HOST" "mkdir -p $ROOT/docs"
scp -q "$TMP/index.html" site/.htaccess site/plugins.json "$HOST:$ROOT/"
scp -q "$TMP/docs/index.html" "$HOST:$ROOT/docs/"
echo "deployed https://sasanokusa.com/sasacode/ ($VERSION)"
