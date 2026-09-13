import { invoke } from "@tauri-apps/api/core";
import { ref, shallowRef } from "vue";
import type { Snapshot, Status, Notice } from "./types";
// Snapshots arrive as immutable JSON; avoid proxying every server and setting.
export const snapshot = shallowRef<Snapshot | null>(null);
export const status = ref<Status>({
  phase: "disconnected",
  error: null,
  seconds: 0,
  upload: 0,
  download: 0,
  profile_id: null,
  profile_name: null,
  logs_revision: 0,
  network_busy: false,
  vpn_requested: false,
});
export const notices = ref<Notice[]>([]),
  busy = ref("");
let noticeId = 0;
export function notice(text: string, error = false, retry?: () => void) {
  const id = ++noticeId;
  notices.value = [...notices.value.slice(-2), { id, text, error, retry }];
  if (!error) setTimeout(() => dismiss(id), 5000);
}
export function dismiss(id: number) {
  notices.value = notices.value.filter((n) => n.id !== id);
}
export const request = <T = unknown>(
  action: Record<string, unknown>,
): Promise<T> => invoke("request", { action });
export async function refresh() {
  snapshot.value = await request<Snapshot>({ type: "snapshot" });
}
export async function poll() {
  // Keep property-level dependencies: a traffic tick only updates its readers.
  Object.assign(status.value, await request<Status>({ type: "poll" }));
}
export async function action(
  type: string,
  args: Record<string, unknown> = {},
  message?: string,
): Promise<boolean> {
  if (busy.value && !(type === "disconnect" && busy.value === "connect"))
    return false;
  busy.value = type;
  try {
    const result = await request<Snapshot>({ type, ...args });
    if (result?.profiles) snapshot.value = result;
    await poll();
    if (message) notice(message);
    return true;
  } catch (error) {
    notice(String(error), true, () => {
      notices.value = [];
      void action(type, args, message);
    });
    try {
      await refresh();
      await poll();
    } catch {}
    return false;
  } finally {
    // A connect finishing must not clear a newer, queued disconnect action.
    if (busy.value === type) busy.value = "";
  }
}
