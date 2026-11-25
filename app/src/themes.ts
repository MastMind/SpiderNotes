// UI themes: id (localStorage + data-theme), label (settings), dark flag
// (the CodeMirror editor picks oneDark for dark themes, its light default
// otherwise — the editor cannot use the app's CSS variables).
export interface UiTheme {
  id: string;
  label: string;
  dark: boolean;
}

export const THEMES: UiTheme[] = [
  { id: "dark", label: "Dark", dark: true },
  { id: "light", label: "Light", dark: false },
  { id: "midnight", label: "Midnight", dark: true },
  { id: "forest", label: "Forest", dark: true },
  { id: "paper", label: "Paper", dark: false },
];

export function themeById(id: string | null | undefined): UiTheme {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}
