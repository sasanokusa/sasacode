/**
 * Token counts and context sizes, the same everywhere (footer, /model, exit summary, /usage):
 * 1534 → "1.5k", 12_345 → "12k", 131072 → "128k" (windows are often powers of two),
 * 1_050_000 → "1.05M", 2_300_000_000 → "2.3B".
 */
export function fmtTokens(n: number): string {
  if (n >= 1e9) return `${+(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${+(n / 1e6).toFixed(n < 1e7 ? 2 : 1)}M`;
  if (n >= 1024 && n % 1000 !== 0 && n % 1024 === 0) return `${n / 1024}k`;
  if (n >= 1000) return `${+(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return String(n);
}
