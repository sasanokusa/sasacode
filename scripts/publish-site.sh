#!/bin/sh
# Deploy the sasanokusa.com/sasacode landing page. Releases themselves are served from GitHub:
# /sasacode/install.sh redirects to the installer attached to the latest release.
#   scripts/publish-site.sh [user@host]
set -eu
HOST="${1:-sasa@100.65.215.48}"
ROOT=/var/www/html/sasacode
cd "$(dirname "$0")/.."
ssh "$HOST" "mkdir -p $ROOT/docs"
scp -q site/index.html site/.htaccess site/plugins.json "$HOST:$ROOT/"
scp -q site/docs/index.html "$HOST:$ROOT/docs/"
echo "deployed https://sasanokusa.com/sasacode/"
