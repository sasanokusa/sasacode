# sasacode-plugin-jev-guard

A plugin for [sasacode](https://github.com/sasanokusa/sasacode) (plugin API ^1.8.0): a second opinion on tool calls from TypeSafe's [Jev](https://commandcode.ai/models/jev) before they run. It was bundled with sasacode up to 0.9.5.

```bash
sasacode plugin install sasacode-plugin-jev-guard
```

[日本語](#日本語) · [English](#english)

## 日本語

ルールと権限モードの判定の後に、Jev の判定を重ねる。Jev は文章を返さず、型付きの質問に確率で答えるモデルで、1回あたり数百ミリ秒で済む。

TUI で `/jev on` と打てば、そのセッションで有効になる（`/jev off` で無効。`/resume` した後も保たれる）。いつも使うなら設定に書く：

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

- Jev への接続先は `backend` で選ぶ。指定がなければ、`CMD_API_KEY` か `TYPESAFE_API_KEY` のある方を使う（どちらもキーチェーンの `commandcode` / `typesafe` でもよい）。

  | `backend` | 接続先 | キー | 既定の `model` |
  | --- | --- | --- | --- |
  | `commandcode` | Command Code の Provider API（GOAT 以上のプラン） | `CMD_API_KEY` | `typesafe/jev` |
  | `typesafe` | TypeSafe の API | `TYPESAFE_API_KEY` | `jev-latest` |
  | `openrouter` | OpenRouter の Decisions API（alpha） | `OPENROUTER_API_KEY` | `typesafe/jev-latest` |
  | `vercel` | Vercel AI Gateway | `AI_GATEWAY_API_KEY` | `typesafe-ai/jev` |
  | `chat` | sasacode の任意のモデル（`model` に `"<provider>/<model>"`、省略で現在のモデル） | そのプロバイダーのキー | — |

  OpenRouter と Vercel は、チャット用にキーを持っているだけの人に判定を送らないよう、名前を指定したときだけ使う。`endpoint` と `apiKeyEnv` で接続先の URL とキーの変数を変えられる（プロキシなど）。動作を実際の API で確かめたのは `commandcode` と `chat` だけで、`typesafe` / `openrouter` / `vercel` は公開されている仕様どおりの形で送る（テストは模擬サーバー）。
- `chat` は Jev の代わりにチャットモデルへ同じ質問をし、JSON で答えさせる。確率が較正されていない代用品で、遅い。実際のコマンドで比べると、DeepSeek V4.1 Flash は止めるべき呼び出しを Jev より多く通した（[docs/benchmarks.md](https://github.com/sasanokusa/sasacode/blob/main/docs/benchmarks.md)）。ローカルモデルは1回に数十秒かかり実用的でない。Jev に接続できない環境で、判定の仕組みだけ使いたいとき向け。
- 有効にすると、ツール呼び出しの内容（コマンド、変更されうるパスとその git の状態、ユーザーの直近の依頼）が接続先に送られる。API キーやトークンらしき文字列は、送る前に伏せる（ベストエフォート）。
- 判定を変えるのは次の場合だけ。
  - 確認なしで実行されるはずだった呼び出し（allow ルール、`auto` モード）: 危険なら拒否、要確認なら確認に回す。
  - `agent` モードで確認に回るはずだった呼び出し: 安全なら確認なしで実行する（モデルによる判定の代わり）。Jev が判断を保留したときは、従来どおりモデルが判定する。
  - softDeny の呼び出し: 安全なら、拒否せずユーザーに確認する。
- ユーザーに確認するはずの呼び出しを、Jev が拒否に変えることはない（確認ダイアログに Jev の判定を添える）。deny ルールの呼び出しは Jev に送らない。
- Jev に渡すのは、ハーネスが集めた事実とユーザー自身の依頼だけで、ツールの出力やファイルの中身は渡さない（Jev は状態に紛れ込んだ誘導文に影響されうるため）。
- 読み取りと、作業ディレクトリ内の編集は判定しない。Jev に届かないとき（キーがない、通信エラー、タイムアウト 5 秒）は、Jev なしの判定のまま続ける。
- 閾値は `thresholds`（`allow` 0.8、ヘッドレスでの `headlessAllow` 0.9、`deny` 0.85、`confidence` 0.6、`risk` 0.6、`flag` 0.7、`clear` 0.3）で変えられる。ほかの設定は `timeoutMs`（既定 5 秒、`chat` は 60 秒）、`skip`（判定しないツール。既定 `todo_write`、`task`）。
- `/jev` で状態と直近の判定を見られる。`/jev on` / `/jev off` でこのセッションの有効・無効を切り替える（キーがなければ有効にせず理由を示す）。判定はセッションにも記録する。
- Jev はコマンドの文字列から判断するので、`ssh host 'ps aux'` のような他のマシンでの読み取りは安全と見る。また `bun run clean` のようにスクリプトの中身が見えない呼び出しは、中身が危険でも気づけない。他のマシンへの操作は `guard` プリセットが常に確認に回す。スクリプト経由の呼び出しも必ず確認したいなら、ルールで決める（例: `"permissions": { "ask": ["bash(bun run *)", "bash(make *)"] }`）。
- 実際のセッションログ 5,135 件での結果: 安全な呼び出しの 92% が auto で止められず、agent モードでは 71% が確認なしで通る。要確認のものが agent で確認なしに通ったのは 1%（ローカル）。詳細は [docs/benchmarks.md](https://github.com/sasanokusa/sasacode/blob/main/docs/benchmarks.md)。

## English

Adds Jev's judgment after the rules and the permission mode. Jev answers typed questions with probabilities instead of text and takes a few hundred milliseconds a call.

Type `/jev on` in the TUI to turn it on for the session (`/jev off` turns it off; the choice survives `/resume`). To have it always, put it in the config:

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

- `backend` picks where Jev is reached. Without it, whichever of `CMD_API_KEY` and `TYPESAFE_API_KEY` is set is used (the keychain entries `commandcode` / `typesafe` work too).

  | `backend` | Endpoint | Key | Default `model` |
  | --- | --- | --- | --- |
  | `commandcode` | Command Code's Provider API (GOAT plan or above) | `CMD_API_KEY` | `typesafe/jev` |
  | `typesafe` | TypeSafe's API | `TYPESAFE_API_KEY` | `jev-latest` |
  | `openrouter` | OpenRouter's Decisions API (alpha) | `OPENROUTER_API_KEY` | `typesafe/jev-latest` |
  | `vercel` | Vercel AI Gateway | `AI_GATEWAY_API_KEY` | `typesafe-ai/jev` |
  | `chat` | Any sasacode model (`model`: `"<provider>/<model>"`, the current model if omitted) | that provider's key | — |

  OpenRouter and Vercel are used only when named, so people who hold those keys for chat do not start sending judgments. `endpoint` and `apiKeyEnv` change the URL and the key variable (for proxies). Only `commandcode` and `chat` have been checked against the real APIs; `typesafe` / `openrouter` / `vercel` send what their published specs describe (tested against mock servers).
- `chat` asks a chat model the same questions and has it answer in JSON. It is an uncalibrated and slow stand-in: on real commands, DeepSeek V4.1 Flash let through more calls that should have been stopped than Jev ([docs/benchmarks.md](https://github.com/sasanokusa/sasacode/blob/main/docs/benchmarks.md)), and local models take tens of seconds a call. It is for when Jev cannot be reached but you want the mechanism.
- When enabled, the call (command, paths it may change and their git state, your latest request) is sent to the endpoint. Strings that look like API keys or tokens are masked first (best effort).
- It changes a decision only in these cases:
  - A call that would run without asking (allow rules, `auto` mode): dangerous → denied, needs a look → asks.
  - A call that `agent` mode would ask about: safe → runs without asking (instead of the model's judgment). When Jev holds back, the model judges as before.
  - A softDeny call: safe → asks you instead of refusing.
- Jev never turns a call you would be asked about into a refusal (its verdict is added to the approval dialog). Calls denied by a rule are not sent to Jev.
- Jev gets only facts the harness collected and your own request, never tool output or file contents (Jev could be steered by instructions hidden in them).
- Reads and edits inside the working directory are not judged. When Jev cannot be reached (no key, network error, 5-second timeout), the decision stands without it.
- Thresholds are set with `thresholds` (`allow` 0.8, headless `headlessAllow` 0.9, `deny` 0.85, `confidence` 0.6, `risk` 0.6, `flag` 0.7, `clear` 0.3). Other settings: `timeoutMs` (default 5 seconds, 60 for `chat`) and `skip` (tools never judged; default `todo_write`, `task`).
- `/jev` shows the state and the latest judgments. `/jev on` / `/jev off` switch it for this session (without a key it stays off and says why). Judgments are also recorded in the session.
- Jev judges by the command line, so a read on another machine such as `ssh host 'ps aux'` looks safe, and a call whose script it cannot see, such as `bun run clean`, is judged without knowing what the script does. The `guard` preset always asks before operations on other machines. To always be asked about scripts, write a rule (e.g. `"permissions": { "ask": ["bash(bun run *)", "bash(make *)"] }`).
- On 5,135 calls from real session logs: 92% of the safe calls are not stopped in `auto`, 71% run without asking in `agent`, and 1% of the calls that need a look ran without asking in `agent` (locally). Details in [docs/benchmarks.md](https://github.com/sasanokusa/sasacode/blob/main/docs/benchmarks.md).
