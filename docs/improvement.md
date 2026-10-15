# 改善の回し方

このハーネスは、作者自身が開発に使えることをM1の完了条件にした（[docs/milestones.md](milestones.md)）。同じ理由で改善も、エージェントか人が1周ずつ回せる形にしておく。1周は小さく、検証つきで、記録を残す。

ここには、1周の手順、どの観点で探すか、いま何を検査しているか、これまでに何を変えたかを置く。ハーネス自身に回させるなら `.sasacode/skills/harness-improve` を読ませる。

## 1周の流れ

1. **計測する**。`bun run improve` が検査の合否、コアの行数、リポジトリの規模を出す。必要なら `bun test` と `bun run typecheck` も走らせる。
2. **観点を1つ選ぶ**。下の表から選ぶ。落ちている検査、余白の小さい予算、片方のREADMEにしかない記述など、信号のあるものから手を付ける。
3. **直す**。変更は1つの観点に絞る。振る舞いが変わるならテストを同時に足す（`packages/<パッケージ>/test`）。プロバイダーの応答は `@sasacode/testing` の録画再生でモデルなしに再現できる。
4. **検証する**。`bun run typecheck && bun test`、`bun run improve`。CLI・TUI・プラグインの見え方が変わるなら、ヘッドレス（`sasacode -p`）かTUIで実機を1回通す。
5. **記録する**。下の「記録」に1行足す。何を変え、どう確かめたかを書く。
6. **コミットする**。1改善1コミット。メッセージは既存の履歴と同じ形（`area: 何を変えたか、どう確かめたか`）にする。

## 観点

| 観点 | 問い | 見るところ |
| --- | --- | --- |
| コアの小ささ | その機能はコアに要るか。プラグインで足りないか | `bun run improve` の行数（上限4,000行、超えるとテストが落ちる）。コアに行を足すときは、同程度を削るか、置き場所を変える |
| テスト | その振る舞いはテストで守られているか。実機でしか見ていない経路はないか | `packages/*/test`。録画再生でループ・プラグイン・権限をモデルなしでE2Eにできる |
| 約束の検査 | 要求やREADMEが約束しているのに、誰も検査していないことはないか | `scripts/checks.ts`。新しい約束は、先に検査を書いてからREADMEに書く |
| ドキュメント | README（日英）とdocsは実装と合っているか。書いてあるのに辿れないdocはないか | `bun run improve` の `readme-cli` / `readme-plugins` / `readme-shape` / `api-version` / `docs-links` / `docs-reachable` |
| 開発体験 | よく使う手順は1コマンドで済むか。失敗したときの出力から直し方が分かるか | `bun run improve`、`bun run typecheck`、`bun test`、`bun run build` |
| 安全性 | 権限の判定、シークレット、プロジェクトの信頼に穴はないか | `packages/agent/test/permission*.test.ts`、`bash` / `read` の実機、[docs/requirements.md](requirements.md) の非機能要件 |
| 可搬性 | macOS / Linux / Windows の差で壊れるところはないか | CI（ubuntu / windows）、Git Bash まわりのテスト、パスの正規化 |
| 速度 | 起動・描画・テストは速いか | 起動から入力可能まで（目標300ms）、`bun test` の所要時間、描画の差分 |

## 検査

`scripts/checks.ts` の検査は `bun test`（`scripts/checks.test.ts`）から走るので、CIでも自動で見る。検査を足すときは、`scripts/checks.test.ts` に「壊れた入力を渡すと落ちる」テストも書く。静かに効かなくなった検査は、検査がないより悪い。

| 検査 | 守っている主張 |
| --- | --- |
| `core-line-budget` | コア（ai・agent・plugin-api・tools）は4,000行未満（README「開発」） |
| `dependency-record` | 実行時の依存パッケージはすべて [docs/dependencies.md](dependencies.md) に理由つきで載っている（[docs/requirements.md](requirements.md) の非機能要件） |
| `readme-cli` | CLIのオプションは両方のREADMEに書いてある |
| `readme-plugins` | 同梱プラグインの一覧とREADMEの表が一致する |
| `readme-shape` | 日英のREADMEは同じ形（見出しの階層とコードブロックの並び）を保つ |
| `api-version` | プラグインAPIの版は、コード・docs/plugins.md・両READMEで一致する |
| `docs-links` | markdownの相対リンクが解決する |
| `docs-reachable` | `docs/*.md` は両方のREADMEから辿れる |

## 記録

| 日付 | 何を | どう確かめたか | コミット |
| --- | --- | --- | --- |
| 2026-10-15 | 約束を検査にする：コアの行数上限、実行時依存の記録、READMEのオプションと同梱プラグイン、相対リンク、docsの到達性（`scripts/checks.ts`） | このリポジトリで6つの検査が通ること、壊れた入力を渡すと落ちることを `scripts/checks.test.ts` のfixtureで確認。`bun test` に組み込まれ、当時の全414件が通る | 693fe60 |
| 2026-10-15 | 検査が見つけたずれの修正：両READMEに `-h, --help` / `-v, --version` の行、`docs/api-compatibility.md` と `docs/benchmarks.md` への導線、コアの行数の書き方を検査に合わせる | 修正前に落ちていた4件が消えること。日英のREADMEがどちらも363行で揃うこと | 4d55e28 |
| 2026-10-15 | `bun run improve` で1周の計測を1コマンドに（検査・コアの行数・規模・次の一手） | 検査が通るときに終了コード0と余白（4,000行まで残り9行）を出し、READMEにずれを作ると1になること | 8c1290b |
| 2026-10-15 | 改善ループをハーネス自身が回せるようにする（`.sasacode/skills/harness-improve`、この文書） | CLIがスキルの一覧を作るのと同じ `discoverSkills` が name と description を読めること。この文書が両READMEから辿れること（`docs-reachable`） | ca3e054 |
| 2026-10-15 | 日英のREADMEの形（見出しの階層とコードブロックの並び）が揃っていることを検査に追加（`readme-shape`） | `README.en.md` から見出しを1つ消すと「36 ... 35」を報告して終了コード1、戻すと0。fixtureでも確認 | c7fd99d |
| 2026-10-15 | プラグインAPIの版の記述を実装に合わせる（両READMEが1.10.0のまま、コードとdocs/plugins.mdは1.13.0）＋検査 `api-version` | 修正前に `bun run improve` が両READMEについて「1.13.0を書いていない」と落ち、直すと通ること。`1.10.0` と `1.2.0` の比較をfixtureで確認 | 438f305 |

## 次の候補

まだ検査にも改善にもなっていないもの。信号が出たら手を付ける。

- 起動から入力可能まで300ms以内という目標は、手元の体感だけで測っていない
- CIはubuntuとwindowsのみ。macOSはリリースのワークフローでしか起動していない
- 日英のREADMEの本文の対応は目視。見出しとコードブロックの並びは検査しているが、段落や表の行が片方にしかないことは検査していない
- `plugins/*` のREADMEと実装のずれは検査していない
- `sasacode --help` の `plugin publish` の行は、`sasacode plugin` の使い方より短い（`--version` / `--api` / `--description` / `--otp` を出さない）。要約として意図したものか確かめてから直す
- `npm/sasacode` のランチャーはテストがあるが、公開後の更新は手作業
- 単一バイナリ（`bun run build`）での起動確認は手元とリリース時のみ
