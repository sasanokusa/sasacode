#!/bin/sh
# The TypeScript sasacode CLI (the reference) as one command, so an e2e suite can be pointed at it
# the way it is pointed at the Rust binary:
#   SASACODE_E2E_BIN=$PWD/crates/sasacode-testing/scripts/sasacode-ts.sh cargo test -p sasacode-testing --test cli_e2e
# Needs bun on the PATH. Remove together with packages/ at the end of the rewrite.
root=$(cd "$(dirname "$0")/../../.." && pwd)
exec bun --no-env-file --config="$root/scripts/no-bunfig.toml" "$root/packages/cli/src/main.ts" "$@"
