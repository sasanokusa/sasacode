# 公式プラグイン

各ディレクトリは独立したnpmパッケージで、`sasacode-plugin-<名前>` として公開済みです。本体には同梱していません。`sasacode plugin install sasacode-plugin-<名前>` で導入します。

| プラグイン | 用途 | 最初の試し方 |
| --- | --- | --- |
| [ask-user](ask-user/README.md) | 推測で進めず、選択肢つきの質問で判断を仰がせる | 「方針をask_userで聞いてから進めて」 |
| [git-context](git-context/README.md) | レビューのための差分収集を1回で済ませる | 「git_contextで変更を確認して」 |
| [project-checks](project-checks/README.md) | 検査の呼び方とログの形式を揃える | `/checks` で確認し、検査の実行を依頼 |
| [workflows](workflows/README.md) | チームや個人の定型依頼を再利用する | `/workflow show review` → `/workflow review` |

まず単独で導入するなら **project-checks**、git-context・project-checks・workflowsの3本を組み合わせるなら **workflows** を手始めにレビューや調査に使えます。`ask-user` と `git-context` はAPI ^1.8.0、ほかの2本は新API ^1.9.0 の公開APIのみを使い、ビルド不要、実行時の追加npm依存なしです。既存の [jev-guard](jev-guard/README.md) は別パッケージです。

## ローカルで試す

リポジトリのルートで必要なものだけインストールし、sasacodeを再起動します。

```sh
sasacode plugin install ./plugins/project-checks --project
sasacode plugin install ./plugins/ask-user --project
sasacode plugin install ./plugins/git-context --project
sasacode plugin install ./plugins/workflows --project
```

開発時の検証は次のとおりです。

```sh
bun test plugins/ask-user plugins/git-context plugins/project-checks plugins/workflows
bun run typecheck
npm pack ./plugins/ask-user --dry-run --ignore-scripts
npm pack ./plugins/git-context --dry-run --ignore-scripts
npm pack ./plugins/project-checks --dry-run --ignore-scripts
npm pack ./plugins/workflows --dry-run --ignore-scripts
```

`bun run pack:plugins` で4本の実際の `.tgz` を `dist/plugin-candidates/` に作成し、展開した配布物をハーネスのローダーで読み込む検証まで行えます（npmとtarが必要）。公開や依存インストールは行いません。

各package.jsonの `files` で配布対象を限定し、テストはパッケージに含めません。個別のLICENSEを同梱しています。実際の配布物を検査するなら `--dry-run` を外して `.tgz` を作り、内容を確認してください。

## 新しい版を公開する

package.jsonの `version` を上げ、manifestの `apiVersion` が使っているAPIと合っているか確認します。npmログイン済みの端末から、そのプラグインのディレクトリで実行します。

```sh
cd plugins/project-checks
npm publish --dry-run --ignore-scripts
npm publish --access public --ignore-scripts
```

新しいプラグインを公開したら `site/plugins.json` に載せ、`scripts/publish-site.sh` でサイトに反映します。
