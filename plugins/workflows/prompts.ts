export interface Workflow { description: string; prompt: string }
export function builtins(lang: "ja" | "en"): Record<string, Workflow> {
  return lang === "ja" ? {
    review: {
      description: "変更差分をレビューする",
      prompt: "現在の変更をレビューしてください。{{input}}\nまず対象の差分と周辺コードを読み、動作不良、回帰、データ損失につながる問題を優先してください。git_context があれば活用してください。指摘は根拠となるファイル・行と再現条件を添え、確実な問題と未確認の懸念を区別してください。指摘がなければその旨と未検証の範囲を示してください。レビューのためにコードを書き換えたりコミットしたりしないでください。",
    },
    diagnose: {
      description: "失敗を再現して原因を調べる",
      prompt: "次の問題を調査してください。{{input}}\nエラーや失敗条件を確認し、利用できる project_checks またはプロジェクトの文書から必要最小限の再現手順を選んでください。実行時の権限確認には従ってください。観測事実と仮説を分け、原因を絞り込んで最小限の修正案と検証方法を提示してください。まだ修正は適用せず、再現できなければ試したことと不足情報を報告してください。",
    },
    "release-check": {
      description: "公開前のパッケージ内容と検証状況を確認する",
      prompt: "このプロジェクトの公開準備を確認してください。{{input}}\nプロジェクトの公開手順とマニフェストを読み、版、配布対象、README、ライセンス、依存関係、必要なビルド・検査を確認してください。既存の検査ツールを使い、実行済みと未実行を区別してください。パッケージの dry-run でも lifecycle script が動く可能性を調べてから実行してください。公開・push・タグ作成・バージョン変更はせず、公開判断に必要な問題点と次のコマンドを報告してください。",
    },
  } : {
    review: { description: "Review the current diff", prompt: "Review the current changes. {{input}}\nRead the diff and surrounding code; use git_context if available. Prioritize bugs, regressions and data loss. Give file/line references and triggering conditions. Separate confirmed issues from uncertainties. If no issues are found, say so and identify unverified areas. Do not modify code or commit while reviewing." },
    diagnose: { description: "Reproduce a failure and investigate its cause", prompt: "Investigate this problem: {{input}}\nRead the failure and choose the smallest reproduction from project_checks or project documentation. Respect execution permissions. Separate observations from hypotheses, narrow down the cause, and propose a minimal fix and validation. Do not apply the fix yet. If reproduction fails, report what you tried and the missing information." },
    "release-check": { description: "Check package contents and release readiness", prompt: "Check release readiness for this project. {{input}}\nRead its release procedure and manifests. Check versions, packaged files, README, license, dependencies, build and required checks. Distinguish executed checks from pending ones. Inspect lifecycle scripts before running a package dry-run, as those scripts may execute. Do not publish, push, tag, or change versions. Report blockers and the next commands needed for the release decision." },
  };
}
