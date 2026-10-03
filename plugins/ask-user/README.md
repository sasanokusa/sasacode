# sasacode-plugin-ask-user

モデルが推測で進めずに、選択肢つきの質問であなたに判断を仰げるようにするsasacodeプラグイン。API ^1.8.0、ビルド不要、追加依存なし。

## 試す

```sh
sasacode plugin install ./plugins/ask-user --project
```

sasacodeを再起動し、「DBの選び方をask_userで聞いてから進めて」のように依頼します。公開後は `sasacode plugin install sasacode-plugin-ask-user` で導入できます。

## 動き

- `ask_user` ツールは1回で1〜4問を尋ねます。1問あたりの選択肢は2〜6個です。質問は順に選択パネルへ表示されます。
- 選択肢が排他的でない質問では、モデルが `multiSelect` を指定できます。spaceで複数を選び、enterで決定します（API 1.10 以降の本体）。
- どの質問にも最後に「その他」が付きます。API 1.10 以降の本体では、その場で自由に答えを入力できます。1.8 / 1.9 の本体では「その他（チャットで答える）」になり、選ぶとそこで打ち切ります。モデルにはターンを終えてあなたの返信を待つよう伝えます。
- Escで閉じた場合、空のまま入力を確定した場合、複数選択で何も選ばずに確定した場合は、そこで打ち切ります。モデルには、答えを推測しないよう伝えます。
- 実行中に中断すると、ツールはエラーで終わります。
- 非対話実行（headless）では何も尋ねません。モデル自身が判断し、置いた前提を返信に書くよう返します。

## 権限

質問するための承認を求めても意味がないので、`ask_user` のallowルールを追加します。askモードでも確認なしで表示されます。止めたい場合は、設定の `permissions.deny` に `ask_user` を書いてください。ユーザーのdenyルールが優先されます。

## 対応する本体

manifestの要求はAPI ^1.8.0 です。自由入力（`ui.input`）と複数選択（`ui.selectMany`）は、本体にあるときだけ使います。1.8 / 1.9 の本体では単一選択とチャットでの回答になります。

## English

Adds an `ask_user` tool: the model asks 1-4 multiple-choice questions (2-6 options each) in the selection panel instead of guessing. Questions may allow several answers (`multiSelect`). Every question offers "Other": on API 1.10+ hosts you type the answer on the spot; on 1.8/1.9 hosts it ends the call and tells the model to wait for your message in the chat. Dismissing a question tells the model not to assume an answer. Headless runs ask nothing and tell the model to decide and state its assumption. Adds an allow rule for `ask_user`, so asking needs no approval; a user deny rule still wins. API ^1.8.0, no build and no dependencies.

Apache-2.0.
