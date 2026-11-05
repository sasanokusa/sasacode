# API互換性の確認表

確認日: 2026-10-01。登録済みのAPI実装はAnthropic Messages、OpenAI Chat Completions、OpenAI Responsesと、同梱プラグインのCodex Responses。プロバイダー名やモデル一覧は設定と`/models`により増えるため、この表は全モデルの実通信保証ではない。HTTP/SSEモックで契約を検証し、実APIを検証した範囲は末尾に分けている。

## 対応経路と保存する状態

| 経路 | 思考・履歴・ストリーム | キャッシュ/互換性と検証範囲 |
| --- | --- | --- |
| Anthropic公式`https://api.anthropic.com` / Messages | 署名付き`thinking`、空の署名付きthinking、`redacted_thinking`を保持。モデル/API/provider/endpointが一致するときだけ再送。署名/prefix不一致の明示的400だけ、thinkingを落として1回再試行 | 公式URLのorigin完全一致だけdisabled/eager input streaming/binding betaを付ける。拒否された機能をendpoint+model別に覚える。input/read/writeは独立。refusal、打切り、abortをモック検証 |
| OpenAI公式Responses / `openai` | 元のreasoning item全体と順序、各assistant messageの`phase`を保持。`store:false`。effortを指定する場合は互換サーバー向けに`reasoning.encrypted_content`を明示要求し、利用者のinclude/reasoning制御と合成 | 現在の公式APIはstore:falseで暗号化推論を自動で返すため、includeがないこと自体は不具合ではない。cached/writeを総inputから分ける。refusalを表示、最初のstreamイベントでのeffort拒否だけ交渉。途中の失敗は再生成しない。モック検証 |
| OpenAI Chat / `openai-chat` | 標準tool_calls/content。サーバーが返した`reasoning` / `reasoning_content`を保存。通常のOpenAIモデルへvendor用必須履歴フィールドを勝手に追加しない。ストリームのtool_callsは`index`で引き、`index`のないchunkは空でない`id`で引く（どちらもなければ直前の呼び出しの続き。新しい`id`か、直前の呼び出しと違う名前、または引数のJSONが閉じた呼び出しの後の名前は別の呼び出し。同じ名前を毎chunkで繰り返すサーバーでも、引数の途中では分けない。名前だけのchunkも同じ扱いなので、呼び出しの最後に名前だけのchunkを送るサーバーがあれば、引数のない呼び出しが1つ増える。そうしたサーバーは知られていない）。後のchunkで届いた名前は空の名前を埋める | effort/noneの拒否を狭く判定。履歴の不足や署名エラーではeffortを下げない。出力上限は、プロバイダーにローカルサーバーのフラグ（`ollama` / `llamacpp` / `lmstudio`）があれば`max_tokens`、なければ（OpenAI、OpenRouter、Command Code、vLLMなど）`max_completion_tokens`で送る。モック検証 |
| OpenRouter Chat / `openrouter` | `reasoning_details`の連続した全chunkを元の形・順序で保存し再送。署名/暗号化を表示用plaintextで代用しない。structuredがない場合はplain reasoningを使用。統一`reasoning:{effort}` | 厳密に`https://openrouter.ai`のoriginだけでこの形式を選ぶ。cached/cache_write/costの報告を読み取る。providerが切り替わる挙動をモックで再現。上流の同一providerへの固定は保証しない |
| Command Code Chat / `commandcode` | OpenAI互換Chat。対応modelのDeepSeek/MiMo履歴を保持 | `/provider/v1`。実効機能はrouter/モデルに依存。旧Claude403はこの経路の問題を公式Anthropicの問題と同一視しない。新規実通信は未検証 |
| Command Code Responses / `commandcode-responses` | 上記Responses契約と同じ | `/provider/v1`。phaseや暗号化推論を下流が通すかは未検証。unsupported明示エラーだけ狭くfallback |
| Command Code Messages / `commandcode-anthropic` | Anthropic互換signed replayとbinding-error fallback | `/provider`。公式限定disabled/eager/betaを送らない。公式機能の下流保証は未検証 |
| Codex / `openai-codex`同梱plugin | Responses実装へ委譲。plan認証/session header、未対応body field除去、session prompt_cache_keyをpluginで適用 | `chatgpt.com/backend-api/codex`。公開fetch hookと合成して調整後の実wireを診断。auth/model list/body/retryをモック検証。新規ログインやplan実通信なし |
| Ollama / `ollama` | Chatの`reasoning`と`reasoning_content`を読み、同一送信先で実際に観測したfieldを再送。`max_tokens`使用（プロバイダー名ではなく`ollama: true`で判定するので、`endpoint add`で別名を付けたOllamaも同じ） | `/v1`。対応effort/modelは`/models`等のメタデータ依存。公式のResponsesは0.13.3以降のstateless互換。通常経路はChat。コンテキスト長は`/api/show`の`num_ctx`か`/api/ps`の実効値。未ロードで分からないモデルは応答の後に取り直す（分かるまで3回まで）。モック検証。出力上限と`/api/ps`の`context_length`だけ実機で確認（2026-11-05、0.40.14、`gemma4:e4b`）：`max_completion_tokens: 16`は無視されて1214トークン・`stop`、`max_tokens: 16`は16トークン・`length` |
| vLLM / llama.cpp / LM Studio / その他custom OpenAI互換 | Chatの両reasoning fieldに対応。同じendpointで観測したplain fieldだけを保存・再送。Responsesを選んだ場合は上記contract。llama.cppとLM Studioは`max_tokens`、vLLMとその他は`max_completion_tokens`。LM Studioは、検出（`/api/v0/models`が答える）で`lmstudio: true`が付いたプロバイダーか、`modelOverrides`で`server: "lmstudio"`を指定したモデルが対象。llama.cppで`off`のときは、`reasoning_effort: "none"`に加えて`chat_template_kwargs: {"enable_thinking": false}`を送る（b10121より前のllama-serverは`reasoning_effort`を読まず、Qwen3などのテンプレートは`enable_thinking`を読む。`--jinja`のテンプレートが対象。extraBodyの`chat_template_kwargs`があればそちらが優先） | server/template/versionの依存あり。vLLMは旧reasoning_contentからreasoningへ変更。全バージョンへの共通パラメーターを追加しない。llama.cppのコンテキスト超過は400、`type: "exceed_context_size_error"`で、文言は「request (N tokens) exceeds the available context size (M tokens), try increasing it」（古い版は「the request exceeds the available context size. try increasing the context size or enable context shift」）。型と`context size`の文言のどちらでも`ContextOverflowError`にし、圧縮に回す。コンテキスト長はllama.cppが`/props`、LM Studioが`/api/v0/models`の`loaded_context_length`。LM Studioの`max_context_length`は参考表示だけで、応答の後に取り直す。フラグのない追加済みOpenAI形式サーバー（LM Studioを見分ける前の設定）も、一覧取得のときに`/api/v0/models`で見分ける。モック検証。llama-serverは超過エラーの形だけ実機で確認（2026-11-05、b9860、100Mモデル）。LM Studioは未起動 |
| DeepSeek native互換 / GOATの`deepseek/deepseek-v4.1-flash` | toolを送るとき全assistant reasoning_contentを保持、plain assistantや次user turnを跨いでも残す。native `deepseek-flash` / `deepseek-v4-flash` / `deepseek-v4.1-flash`も対象 | toolsなしはサーバーが無視するreasoningを省く。native hit/miss usage aliasも読む。GOATは以前の修正版実行でmulti-turn再送を確認。native直結は未検証 |
| MiMo互換 / GOATの`xiaomi/mimo-v2.6-flash` | tool-call assistantのreasoning_contentを全て保持。次user turnやtoolsを外した後にも履歴を残す | native docsには「推奨」と「必須」の記述差があるためtool-call continuityを保守的に保持。GOATは以前の修正版実行で再送を確認。native直結は未検証 |
| Gemini / Azure / Bedrock固有API | 固有transportは未登録。互換routerまたは利用者のProvider plugin経由 | native認証・専用schemaの動作保証なし |

## 再送の範囲

新しいassistant messageはAPI・provider・model ID・実効baseURLのSHA-256 digestを保存する。再開後もこの一致が必要で、endpoint/model/API/provider切り替え時は署名/暗号化推論を送らない。秘密値や署名をdigestとして使わない。旧セッションは送信先の来歴がないため推論状態を再送せず、可視本文/tool履歴を維持する。権限・課金・認証アカウントの確認をこのdigestで代用することはできない。

Anthropicのprefix bindingはsystem/tools/historyや組織変更にも依存する。安定したprefixを保ち、`drop_block`が成功しても推論再利用を保証しない。診断で`prefix_binding_mismatch` / `organization_binding_mismatch`の破棄件数を確認できる。Claude Mythos 5.1はprefix checkを持つモデルとして扱わない。Sonnet 5.5のoffはbetween_tools、それ以外の対応モデルはdisabledまたはlow effortにする。APIがその制御を拒否した場合だけ交渉する。

base systemは短いidentityと環境事実だけに保つ。append/AGENTS.mdの指示は環境より先、変動するcwdは末尾。任意のsystem_prompt pluginは自分で順序を管理する。prefixが安定しても、キャッシュの最小長・routing・期限によりhitするとは限らない。明示breakpoint/retention/provider固定の新しい設定はこの修正では導入しない。

## 任意のJev guard経路

`plugins/jev-guard`はstatelessの判断で、signed思考を再送しない。

| サービス | 送信先 / モデル | 特有の契約 |
| --- | --- | --- |
| Command Code | `/provider/v1/systemone` / `typesafe/jev` | native answers choice/noulとconfidence |
| TypeSafe | `api.typesafe.ai/v1/systemone` / `jev-latest` | 同じnative schema |
| OpenRouter Decisions | `/api/alpha/decisions` / `~typesafe/jev-latest` | chat endpointではない。公式のlatest aliasへ修正 |
| Vercel AI Gateway | `/v4/ai/evaluation-model` / `typesafe-ai/jev` | modelはheader、noulはboolean/probability、confidenceはproviderMetadata。欠落時は0として自動許可しない |
| chat stand-in | `api.agent.complete` / 選択model | 通常chatの自己申告confidenceはcalibratedではない |

429/529/5xxは一回まで、Retry-AfterとcallerのAbortSignalに従う。待ちが30秒を超える場合は早期再送せずcallerの失敗ポリシーへ返す。validation/authの4xxは再試行しない。全経路をモック検証、Jev課金APIは実行していない。

## 検証の限界とベンチマーク

以前の修正版でAnthropic公式のsigned replay・prefix drop・cache・effortを実検証し、GOATの2モデルをsasacode/OpenCodeで比較した。今回は追加の生成API呼び出しを行わず、仕様確認とHTTP/SSEモック、ローカル全テストを実施する。refusalはモックのみ。以前の測定は今回の全変更の性能を実証するものではない。

GOATで確認された正確なmodel IDは`xiaomi/mimo-v2.6-flash`、`deepseek/deepseek-v4.1-flash`。比較基盤はworkspaceのpilotにあり、sasacodeとOpenCodeの6課題・同一PATH/rg・同一model・検証コマンド・token ledgerを使用した。ただしnative tool schemaは4対8、測定時刻/キャッシュ条件が異なり、各課題1回なので一般的品質差は主張しない。次回は最終commit/binaryを固定、実行順を交互/無作為化、cold/warmを分け、複数反復とheld-out課題を追加し、成功率・fee/成功・経過時間・reasoning/キャッシュ内訳・tool失敗・実wire試行を並べる。他候補はCodex CLIとClaude Codeだが、選んだrouter/modelを同条件で指定できるか先に確認する。課金上限・利用先・追加ハーネスは承認後に実施する。

## 公式仕様の参照

- [Claude preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking): prefix/org binding、drop_block、input_transformations。
- [OpenAI reasoning](https://developers.openai.com/api/docs/guides/reasoning): stateless encrypted state、phase、連続itemの保持。
- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching): 共通prefix、総inputとread/write counters。
- [OpenRouter reasoning](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens): reasoning_detailsのchunk順序、unified effort。
- [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/)、[KV cache](https://api-docs.deepseek.com/guides/kv_cache/): 全tool履歴とnative cache counters。
- [MiMo deep thinking](https://mimo.mi.com/docs/en-US/quick-start/usage-guide/other/deep-thinking): tool-call thinking continuity。
- [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)、[vLLM reasoning outputs](https://docs.vllm.ai/en/latest/features/reasoning_outputs/)、[llama.cpp server](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)、[LM Studio API changelog](https://lmstudio.ai/docs/developer/api-changelog)、[LM Studio REST API v0](https://lmstudio.ai/docs/developer/rest/endpoints): local serverごとのfieldと対応範囲。
- [Command Code provider](https://commandcode.ai/docs/provider): 3種のwire API。
- [OpenRouter Jev](https://openrouter.ai/blog/tutorials/how-to-use-jev/): Decisions endpoint、latest aliasとconfidenceの意味。
