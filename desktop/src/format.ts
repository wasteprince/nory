export function bytes(value: number | null | undefined): string {
  if (!value || value < 0) return "0 Б";
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    index = Math.min(4, Math.floor(Math.log(value) / Math.log(1024)));
  return `${new Intl.NumberFormat("ru", { maximumFractionDigits: index ? 1 : 0 }).format(value / 1024 ** index)} ${units[index]}`;
}
export function duration(seconds: number): string {
  return [
    Math.floor(seconds / 3600),
    Math.floor(seconds / 60) % 60,
    seconds % 60,
  ]
    .map((n) => String(n).padStart(2, "0"))
    .join(":");
}
export function cleanName(name: string): string {
  return name
    .replace(/(?:[\u{1F1E6}-\u{1F1FF}]{2}|🌐|🌍|🌎|🌏)\s*/u, "")
    .trim();
}
export function flag(name: string): string | null {
  const explicit = name.match(/[\u{1F1E6}-\u{1F1FF}]{2}/u)?.[0];
  if (explicit) return explicit;

  // Providers often omit the emoji and leave only a city in the node title.
  // Keep this small, deterministic fallback so the selected-server card and
  // the grid stay visually consistent without a network lookup.
  const cityFlags: Array<[string, string]> = [
    ["париж", "🇫🇷"], ["paris", "🇫🇷"],
    ["амстердам", "🇳🇱"], ["amsterdam", "🇳🇱"],
    ["франкфурт", "🇩🇪"], ["frankfurt", "🇩🇪"],
    ["милан", "🇮🇹"], ["milan", "🇮🇹"],
    ["тирана", "🇦🇱"], ["tirana", "🇦🇱"],
    ["таллин", "🇪🇪"], ["tallinn", "🇪🇪"],
    ["хельсинки", "🇫🇮"], ["helsinki", "🇫🇮"],
    ["варшава", "🇵🇱"], ["warsaw", "🇵🇱"],
    ["шарлотт", "🇺🇸"], ["charlotte", "🇺🇸"],
    ["москва", "🇷🇺"], ["moscow", "🇷🇺"],
    ["стокгольм", "🇸🇪"], ["stockholm", "🇸🇪"],
    ["лондон", "🇬🇧"], ["london", "🇬🇧"],
    ["хельсингфорс", "🇫🇮"], ["прага", "🇨🇿"], ["prague", "🇨🇿"],
    ["вена", "🇦🇹"], ["vienna", "🇦🇹"], ["цюрих", "🇨🇭"], ["zurich", "🇨🇭"],
    ["варшава", "🇵🇱"], ["токио", "🇯🇵"], ["tokyo", "🇯🇵"],
    ["сингапур", "🇸🇬"], ["singapore", "🇸🇬"], ["нью-йорк", "🇺🇸"], ["new york", "🇺🇸"],
  ];
  const normalized = name.toLocaleLowerCase("ru-RU");
  return cityFlags.find(([city]) => normalized.includes(city))?.[1] ?? null;
}
export function date(value: number | null | undefined): string {
  return value
    ? new Date(value * 1000).toLocaleString("ru", {
        day: "numeric",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
      })
    : "Ещё не обновлялась";
}
