# sasacode-plugin-git-context

レビューの初動で必要なGitの状態・変更統計・パッチを、1回のツール呼び出しで取得するsasacodeプラグイン。API ^1.8.0、Gitが必要です。追加のサービスやキーは不要です。

## 試す

リポジトリのルートで実行し、sasacodeを起動し直してください。

```sh
sasacode plugin install ./plugins/git-context --project
```

公開後は `sasacode plugin install sasacode-plugin-git-context` でインストールできます。

「git_contextを使って変更をレビューして」と依頼すると利用できます。設定は不要です。

## ツール

`git_context` の引数は次のとおりです。

| 引数 | 既定 | 意味 |
| --- | --- | --- |
| `scope` | `working` | `working`: HEADと作業ツリーの差分（ステージ済み＋未ステージ）。`staged`: HEADとindex。`branch`: merge-baseとHEAD |
| `base` | `@{upstream}` | `branch` の比較元。例 `main` / `origin/main`。リモート追跡ブランチはローカルにある版を使用 |
| `patch` | `true` | falseなら状態と統計だけ取得 |
| `maxChars` | `16000` | 出力全体の文字数上限、1000〜60000 |

```json
{"scope":"branch","base":"main","maxChars":24000}
```

作業ディレクトリ以下に限定して読み取ります。Gitの外部diff / textconv / fsmonitorは無効化し、変更・fetchはしません。ユーザーの通常のread権限で判定されます。作業ツリーの状態は全モードで表示するため、branchのパッチに含まれない未コミット変更も確認できます。

未追跡ファイルは名前だけ、サブモジュールの差分は対象外です。バイナリはGitの通常の差分表現になります。ステージで変更し作業ツリーで戻した箇所はworkingでは相殺されるので、必要に応じstagedも調べてください。

上限を超えた出力には省略を明記します。各Gitプロセスは15秒・8 MiBまで。これを超える巨大な差分はエラーになり、`patch:false` や個別ファイルの読み取りが必要です。実行中に別プロセスが編集した場合、各セクションの時点は一致しません。upstreamが未設定なら `base` を明示してください。

## English

A read-only `git_context` tool returning bounded status, diff statistics and patches. Supports combined working changes, staged changes, and merge-base-to-HEAD branch review. Requires Git and sasacode plugin API ^1.8.0; no credentials or service. Install locally with the command above, or by package name after publication. Defaults to 16,000 characters; truncation is explicit. Untracked contents and submodule changes are excluded. Branch mode defaults to the locally available upstream; specify `base` if none is configured. Git commands time out after 15 seconds and reject output exceeding 8 MiB.

Apache-2.0.
