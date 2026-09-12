import { ref } from "vue";

export type Theme = "light" | "dark";
const key = "nory.appearance";
function initialTheme(): Theme {
  try {
    const saved = localStorage.getItem(key);
    if (saved === "light" || saved === "dark") return saved;
  } catch { /* Storage can be disabled by the WebView. */ }
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}
export const theme = ref<Theme>(initialTheme());
document.documentElement.dataset.theme = theme.value;
export function setTheme(value: Theme) {
  theme.value = value;
  document.documentElement.dataset.theme = value;
  try { localStorage.setItem(key, value); } catch { /* Keep the current selection. */ }
}
export function toggleTheme() {
  setTheme(theme.value === "dark" ? "light" : "dark");
}
