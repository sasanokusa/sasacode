# sasacode-plugin-workflows

繰り返し使う依頼を `/workflow` から呼び出す sasacode プラグイン。レビューの観点や検査手順を毎回入力する手間を減らします。API ^1.9.0。追加の依存関係はありません。

## 試す

```sh
sasacode plugin install ./plugins/workflows --project
```

sasacode を再起動してください。公開後は `sasacode plugin install sasacode-plugin-workflows` で導入できます。

```text
/workflow
/workflow show review
/workflow review 認証まわりを重点的に
/workflow diagnose 決済テストがタイムアウトする
/workflow release-check plugins/git-context を対象に
```

- `review`: 差分と周辺コードを読み、根拠・再現条件付きでレビュー。コードを変更しない依頼。
- `diagnose`: 失敗を再現し、観測と仮説を分け、修正案を示す。まだ適用しない依頼。
- `release-check`: 公開内容、ライセンス、依存関係、検査結果を確認。公開やバージョン変更はしない依頼。

`/workflow` または `/workflow list` は一覧、`/workflow show <名前> [追加依頼]` は展開結果のプレビューです。プレビューは検索・スクロール・コピーできる長文パネルで表示し、Esc で閉じます。モデルを呼びません。実行時には通常のユーザーメッセージとして現在のセッションへ送り、アイドルなら実行が始まります。実行中ならハーネスのキューに入ります。サブエージェントや追加のモデル呼び出しは作りません。

組み込みの定型文は UI の言語設定（ja/en）に従います。git-context / project-checks があれば活用する依頼ですが、なくても利用できます。これはプロンプト集であり、行動の強制や権限制限を実装するものではありません。実行権限は通常どおりハーネスが判定します。

## 追加・上書き・無効化

`.sasacode/config.json` またはユーザー設定に指定します。名前ごとに組み込みを上書き、false で無効化できます。変更後は再起動してください。

```json
{
  "plugins": {
    "settings": {
      "workflows": {
        "workflows": {
          "release-check": false,
          "explain": {
            "description": "コードの処理経路を説明する",
            "prompt": "{{input}} の処理経路を調べ、入口から出口までファイルと関数名を添えて説明してください。コードは変更しないでください。"
          }
        }
      }
    }
  }
}
```

`/workflow explain ログイン処理` で呼び出せます。プレースホルダーは `{{input}}` のみ。省略した場合、追加依頼を末尾に付けます。シェル、環境変数、ファイル読込は展開しません。名前は英小文字で始まる40文字以内の英小文字・数字・ハイフン（`list` / `show` は予約）。説明は200文字、プロンプトは20,000文字まで。補完に対応しています。

## English

Reusable user requests for sasacode API ^1.9.0. `/workflow review`, `diagnose`, and `release-check` cover review, failure investigation and release readiness. `/workflow show <name>` previews without running. Settings can add/override templates or disable built-ins with false. Only `{{input}}` is expanded, literally and in one pass; no shell or environment expansion. Requests use the current session and the host's normal permissions. These are prompts, not an enforcement mechanism. Built-ins follow the UI language.

Apache-2.0.
