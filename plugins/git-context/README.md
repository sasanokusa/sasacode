# sasacode-plugin-git-context

レビューの初動で必要な Git の状態・変更統計・パッチを、1回のツール呼び出しで取得する sasacode プラグイン。API ^1.8.0、Git が必要です。追加のサービスやキーは不要です。

## 試す

リポジトリのルートで実行し、sasacode を起動し直してください。

```sh
sasacode plugin install ./plugins/git-context --project
```

公開後は `sasacode plugin install sasacode-plugin-git-context` でインストールできます。

「git_context を使って変更をレビューして」と依頼すると利用できます。設定は不要です。

## ツール

`git_context` の引数：

| 引数 | 既定 | 意味 |
| --- | --- | --- |
| `scope` | `working` | `working`: HEAD と作業ツリーの差分（ステージ済み＋未ステージ）。`staged`: HEAD と index。`branch`: merge-base と HEAD |
| `base` | `@{upstream}` | `branch` の比較元。例 `main` / `origin/main`。リモート追跡ブランチはローカルにある版を使用 |
| `patch` | `true` | false なら状態と統計だけ取得 |
| `maxChars` | `16000` | 出力全体の文字数上限、1000〜60000 |

```json
{"scope":"branch","base":"main","maxChars":24000}
```

作業ディレクトリ以下に限定して読み取ります。Git の外部 diff / textconv / fsmonitor は無効化し、変更・fetch はしません。ユーザーの通常の read 権限で判定されます。作業ツリーの状態は全モードで表示するため、branch のパッチに含まれない未コミット変更も確認できます。

未追跡ファイルは名前だけ、サブモジュールの差分は対象外です。バイナリは Git の通常の差分表現になります。ステージで変更し作業ツリーで戻した箇所は working では相殺されるので、必要に応じ staged も調べてください。

上限を超えた出力には省略を明記します。各 Git プロセスは15秒・8 MiBまで。これを超える巨大な差分はエラーになり、`patch:false` や個別ファイルの読み取りが必要です。実行中に別プロセスが編集した場合、各セクションの時点は一致しません。upstream が未設定なら `base` を明示してください。

## English

A read-only `git_context` tool returning bounded status, diff statistics and patches. Supports combined working changes, staged changes, and merge-base-to-HEAD branch review. Requires Git and sasacode plugin API ^1.8.0; no credentials or service. Install locally with the command above, or by package name after publication. Defaults to 16,000 characters; truncation is explicit. Untracked contents and submodule changes are excluded. Branch mode defaults to the locally available upstream; specify `base` if none is configured. Git commands time out after 15 seconds and reject output exceeding 8 MiB.

Apache-2.0.
