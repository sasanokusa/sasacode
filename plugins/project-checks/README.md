# sasacode-plugin-project-checks

テスト・lint・型検査などを名前で実行し、PASS / FAIL、終了コード、実行時間、ログを返すsasacodeプラグイン。API ^1.9.0、bashとプロジェクトで使用する検査コマンドが必要です。macOS / Linux向け。

## 試す

```sh
sasacode plugin install ./plugins/project-checks --project
```

sasacodeを再起動し、`/checks` で利用可能な検査を確認。「project_checksで一覧を確認し、型検査を実行して」と依頼できます。公開後は `sasacode plugin install sasacode-plugin-project-checks` で導入できます。

設定がなければ、起動ディレクトリのpackage.jsonから `test` / `lint` / `typecheck` / `check` / `build` を検出。ルート直下のlockfileでbun / pnpm / yarn / npmを選び、lockfileがなければnpmを使用します。複数のパッケージマネージャーが検出されたら明示設定を求めます。monorepoの親ディレクトリは探索しません。

## 設定

`.sasacode/config.json` またはユーザー設定に記述します。プロジェクト設定はハーネスの信頼確認の対象です。`checks` を指定すると自動検出を置き換えます。空の `{}` は検査なし。非JavaScriptプロジェクトにも使用できます。

```json
{
  "plugins": {
    "settings": {
      "project-checks": {
        "checks": {
          "test": { "command": "bun test", "timeout": 180 },
          "types": "bun run typecheck",
          "lint": "ruff check ."
        }
      }
    }
  }
}
```

名前は英小文字で始まる40文字以内の英小文字・数字・ハイフン。timeoutは秒、既定120、範囲1〜3600。起動時の設定で固定されるので変更後は再起動してください。

- `project_checks`: コマンド一覧を読む。
- `project_check({"name":"types"})`: 検査を1本実行する。並列実行は指定しない。
- `/checks`: 長文表示パネルで同じ一覧を表示。検索・スクロール・コピーに対応。コマンドは実行しない。

## 実行の扱い

`project_check` はexecツールで、`permissionsAs: "bash"` と実際のコマンド文字列を使います。例えば `bash(bun test*)` のdeny / askがそのまま適用されます。自動許可ルールは追加しません。検査スクリプトがファイルを書き換える場合もあるので、読み取り専用とは扱いません。

実行は新API `ctx.callTool("bash", ...)` でホストのbashツールへ委譲します。子ツールにも通常の引数検証・フック・権限判定が適用され、親と子の両方で確認が必要な場合はそれぞれ確認します。bashが無効なら実行できません。設定値は `api.defineSettings()` でも検証します。

組み込みbashは `bash -c` を作業ディレクトリで実行し、標準入力を閉じます。`childEnv()` によりハーネスの秘匿キーを除外し、中断・タイムアウトではプロセスグループを終了します。環境変数は通常のbashと同じです。CIや非watchモードが必要なら、チェックに `CI=1 bun test` のように明示してください。

組み込みbashの出力制限は末尾30,000文字で、超えた分を含むログは本人専用tmpに保存します。保持方針はハーネスに従います。bashを別プラグインで置き換えた場合、実行・ログ処理はそのツールの仕様に従います。時間には承認待ちも含みます。タイムアウト・中断・拒否・非ゼロ終了はFAIL、成功はその1回のツール結果が成功だったことを意味します。

## English

Named checks for sasacode API ^1.9.0 on macOS/Linux. Auto-discovers common package.json scripts using a root lockfile, or accepts explicit `checks` as shown above. `/checks` and `project_checks` list commands; `project_check` runs one check. Commands retain the host's bash permission rules. Delegates to the registered bash tool through the host's validation, hooks, permissions and cancellation. Set CI or one-shot test flags explicitly in the configured command when needed. Reports exit status, duration and bounded output; supports cancellation and timeouts. A PASS applies only to that command and invocation. Requires the relevant project tools to be installed; does not install dependencies.

Apache-2.0.
