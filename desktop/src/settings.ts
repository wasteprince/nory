import type { Settings } from "./types";

// Background subscription/settings changes must not discard edited fields or
// resurrect an outdated routing list when the draft is saved later.
export function mergeSettingsDraft(
  next: Settings,
  previous?: Settings,
  draft?: Settings | null,
): Settings {
  const merged: Settings = JSON.parse(JSON.stringify(next));
  if (previous && draft)
    for (const [key, value] of Object.entries(draft)) {
      if (JSON.stringify(value) !== JSON.stringify(previous[key]))
        merged[key] = JSON.parse(JSON.stringify(value));
    }
  return merged;
}
