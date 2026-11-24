export type ThemeId =
  | "system"
  | "slate-dark"
  | "midnight"
  | "catppuccin"
  | "nord"
  | "clean-light";

export interface ThemeMeta {
  id: ThemeId;
  label: string;
  swatches: [string, string, string];
}

export const THEMES: ThemeMeta[] = [
  { id: "system", label: "System Auto", swatches: ["#0d1117", "#f6f8fa", "#4f8ff7"] },
  { id: "slate-dark", label: "Slate Dark", swatches: ["#0d1117", "#1a212c", "#4f8ff7"] },
  { id: "midnight", label: "Midnight", swatches: ["#07090d", "#131720", "#60a5fa"] },
  { id: "catppuccin", label: "Catppuccin Mocha", swatches: ["#1e1e2e", "#313244", "#89b4fa"] },
  { id: "nord", label: "Nord", swatches: ["#242933", "#2e3440", "#88c0d0"] },
  { id: "clean-light", label: "Clean Light", swatches: ["#ffffff", "#eef2f6", "#2563eb"] },
];

const THEME_KEY = "android-lens:theme";
const FONT_KEY = "android-lens:codeFontSize";
const DEFAULT_FONT = 12.5;

export class AppearanceManager {
  private prefTheme: ThemeId;
  private fontSize: number;
  private media = window.matchMedia("(prefers-color-scheme: dark)");

  constructor(private readonly onChange?: () => void) {
    const saved = localStorage.getItem(THEME_KEY);
    // Migrate legacy "dark"/"light" values smoothly
    if (saved === "dark") this.prefTheme = "slate-dark";
    else if (saved === "light") this.prefTheme = "clean-light";
    else if (saved && THEMES.some((t) => t.id === saved)) this.prefTheme = saved as ThemeId;
    else this.prefTheme = "slate-dark";

    const savedFont = Number(localStorage.getItem(FONT_KEY));
    this.fontSize =
      Number.isFinite(savedFont) && savedFont >= 10.5 && savedFont <= 17
        ? savedFont
        : DEFAULT_FONT;

    this.media.addEventListener("change", () => {
      if (this.prefTheme === "system") {
        this.applyThemeDom();
      }
    });

    this.applyThemeDom();
    this.applyFontDom();
  }

  getTheme(): ThemeId {
    return this.prefTheme;
  }

  getFontSize(): number {
    return this.fontSize;
  }

  setTheme(id: ThemeId) {
    this.prefTheme = id;
    localStorage.setItem(THEME_KEY, id);
    this.applyThemeDom();
    this.onChange?.();
  }

  adjustFontSize(delta: number) {
    this.setFontSize(this.fontSize + delta);
  }

  resetFontSize() {
    this.setFontSize(DEFAULT_FONT);
  }

  setFontSize(size: number) {
    this.fontSize = Math.max(10.5, Math.min(16.5, Math.round(size * 2) / 2));
    localStorage.setItem(FONT_KEY, String(this.fontSize));
    this.applyFontDom();
    this.onChange?.();
  }

  private applyThemeDom() {
    const resolved =
      this.prefTheme === "system"
        ? this.media.matches
          ? "slate-dark"
          : "clean-light"
        : this.prefTheme;
    document.documentElement.setAttribute("data-theme", resolved);
  }

  private applyFontDom() {
    document.documentElement.style.setProperty("--code-font-size", `${this.fontSize}px`);
  }
}
