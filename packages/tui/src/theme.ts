import type { EditorTheme, MarkdownTheme, SelectListTheme } from "@earendil-works/pi-tui";

const sgr = (open: string, close: string) => (s: string) => `\x1b[${open}m${s}\x1b[${close}m`;

export const c = {
  bold: sgr("1", "22"),
  dim: sgr("2", "22"),
  italic: sgr("3", "23"),
  underline: sgr("4", "24"),
  strike: sgr("9", "29"),
  red: sgr("31", "39"),
  green: sgr("32", "39"),
  yellow: sgr("33", "39"),
  blue: sgr("34", "39"),
  magenta: sgr("35", "39"),
  cyan: sgr("36", "39"),
  gray: sgr("90", "39"),
  bgUser: sgr("48;5;236", "49"),
  bgAdd: sgr("48;5;22", "49"),
  bgDel: sgr("48;5;52", "49"),
};

export const selectTheme: SelectListTheme = {
  selectedPrefix: c.cyan,
  selectedText: (s) => c.bold(c.cyan(s)),
  description: c.gray,
  scrollInfo: c.gray,
  noMatch: c.gray,
};

export const editorTheme: EditorTheme = {
  borderColor: c.gray,
  selectList: selectTheme,
};

export const markdownTheme: MarkdownTheme = {
  heading: (s) => c.bold(c.cyan(s)),
  link: c.blue,
  linkUrl: c.gray,
  code: c.yellow,
  codeBlock: (s) => s,
  codeBlockBorder: c.gray,
  quote: c.italic,
  quoteBorder: c.gray,
  hr: c.gray,
  listBullet: c.cyan,
  bold: c.bold,
  italic: c.italic,
  strikethrough: c.strike,
  underline: c.underline,
};
