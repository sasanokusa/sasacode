---
name: harness-improve
description: このリポジトリ（sasacodeハーネス）の改善を1周まわす手順。計測（bun run improve）から、観点を1つ選び、テストを書いて直し、検証し、docs/improvement.mdに記録するまで。改善に取り組むとき、または「ハーネスを改善して」と言われたときに読む。
---

# ハーネスの改善を1周まわす

全体像と観点の表は `docs/improvement.md` にある。ここは手順と、守ること。

## 手順

1. `bun run improve` を実行する。検査の合否、コアの行数（上限4,000）、リポジトリの規模が出る。落ちている検査があれば、それが最優先。
2. 観点を1つ選ぶ（`docs/improvement.md` の表：コアの小ささ / テスト / 約束の検査 / ドキュメント / 開発体験 / 安全性 / 可搬性 / 速度）。1周で1つだけ。
3. 変更する。振る舞いが変わるなら `packages/<パッケージ>/test` にテストを足す。プロバイダーの応答は `@sasacode/testing` の録画再生でモデルなしに再現できる。
4. 検証する。`bun run typecheck && bun test`、`bun run improve`。CLI・TUI・プラグインの見え方が変わるなら、`sasacode -p` のヘッドレス実行で1回通す。
5. `docs/improvement.md` の「記録」に1行足す。日付・何を変えたか・どう確かめたか。
6. 1改善1コミット。メッセージは `area: 何を変えたか、どう確かめたか` の形。

## 守ること

- コア（`packages/{ai,agent,plugin-api,tools}/src`）に行を足すなら、同程度を削るか、置き場所をプラグインに変える。上限は4,000行で、テストが守っている。
- 検査（`scripts/checks.ts`）を足すときは、`scripts/checks.test.ts` に「壊れた入力を渡すと落ちる」テストも書く。静かに効かなくなった検査は、検査がないより悪い。
- `README.md` と `README.en.md` は1つの文書の2言語。片方だけ直さない。
- 確かめていないことを「済み」と記録しない。確かめられなかったことは、「次の候補」に残す。
