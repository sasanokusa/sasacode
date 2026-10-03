# sasacode

A small, pluggable coding agent for the terminal. See the [project README](https://github.com/sasanokusa/sasacode#readme) and [the docs](https://sasanokusa.com/sasacode/docs/).

```bash
npm install -g sasacode
sasacode
```

This package is a launcher. On its first run it downloads the single binary of the same version for your machine (macOS arm64 / x64, Linux x64 / arm64, glibc or musl, Windows x64 / arm64) from [GitHub Releases](https://github.com/sasanokusa/sasacode/releases), checks it against the release's `SHA256SUMS`, and keeps it in `~/.sasacode/npm-bin`. Later runs start it directly. Update with `npm install -g sasacode@latest`. On Windows, the `bash` tool needs [Git for Windows](https://git-scm.com/download/win) (Git Bash).

Without npm: `curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh`.
