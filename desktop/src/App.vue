<script setup lang="ts">
import {
  computed,
  onMounted,
  onBeforeUnmount,
  ref,
  watch,
  nextTick,
} from "vue";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Power,
  Sun,
  Moon,
  ArrowDownLeft,
  ArrowUpRight,
  Globe2,
  Plus,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  RefreshCw,
  Zap,
  Trash2,
  Settings2,
  Route,
  ScrollText,
  Search,
  X,
  Check,
  AlertCircle,
  LoaderCircle,
  ExternalLink,
  FolderOpen,
  Monitor,
  Activity,
  Download,
  SlidersHorizontal,
  Minus,
  Square,
  Copy,
  Braces,
  Link2,
} from "@lucide/vue";
import {
  snapshot,
  status,
  notices,
  busy,
  notice,
  dismiss,
  request,
  refresh,
  poll,
  action,
} from "./api";
import { bytes, duration, date, cleanName, flag } from "./format";
import { mergeSettingsDraft } from "./settings";
import { visibilityPoller } from "./polling";
import { theme, setTheme, toggleTheme } from "./theme";
import { isEditing, subscriptionFromPaste } from "./clipboard";
import type { Settings, Log, CatalogItem } from "./types";
import Toggle from "./Toggle.vue";
import ServerCard from "./ServerCard.vue";
import ProtocolBadges from "./ProtocolBadges.vue";
import WorldMap from "./WorldMap.vue";
import SelectMenu from "./SelectMenu.vue";
import logo from "../../assets/io.nory.NORY.svg";

type Page = "home" | "bypass" | "logs" | "settings";
const page = ref<Page>("home"),
  loading = ref(true),
  startupError = ref(""),
  expanded = ref(true);
const maximized = ref(false);
async function windowAction(action: "minimize" | "toggleMaximize" | "close") {
  if (!isTauri()) return;
  try {
    await getCurrentWindow()[action]();
    if (action === "toggleMaximize")
      maximized.value = await getCurrentWindow().isMaximized();
  } catch (error) {
    notice(`Не удалось изменить окно: ${error}`, true);
  }
}
const adding = ref(false),
  importMode = ref(false),
  subUrl = ref(""),
  sendHwid = ref(true),
  deleting = ref(false);
const subscriptionDialog = ref<"json" | "url" | null>(null);
const subscriptionDetails = ref<{ id: string; name: string; url?: string; text?: string; original?: boolean } | null>(null);
const subscriptionDialogLoading = ref(false), subscriptionDialogError = ref(""), editedUrl = ref("");
let subscriptionDialogRevision = 0;
const catalogOpen = ref(false),
  catalogMode = ref<"apps" | "processes">("apps"),
  catalog = ref<CatalogItem[]>([]),
  catalogSearch = ref(""),
  catalogLoading = ref(false);
const logs = ref<Log[]>([]),
  logQuery = ref(""),
  logLevel = ref("all"),
  settingsCategory = ref("connection"),
  draft = ref<Settings | null>(null);
const update = ref<{ version: string; notes: string } | null>(null),
  updateBusy = ref(false),
  updateProgress = ref("");
const nowConnected = computed(() => status.value.phase === "connected");
const vpnRequested = computed(
  () => status.value.vpn_requested || nowConnected.value || busy.value === "connect",
);
const canCancelConnecting = computed(
  () => busy.value === "connect" || (!busy.value && status.value.phase === "connecting"),
);
const working = computed(
  () =>
    !!busy.value ||
    ["connecting", "disconnecting"].includes(status.value.phase),
);
const currentSub = computed(() =>
  snapshot.value?.subscriptions.find(
    (s) => s.id === snapshot.value?.selected_subscription,
  ),
);
const selected = computed(() =>
  snapshot.value?.profiles.find(
    (p) => p.id === snapshot.value?.selected_profile,
  ),
);
const shownProfile = computed(() =>
  nowConnected.value
    ? snapshot.value?.profiles.find((p) => p.id === status.value.profile_id)
    : selected.value,
);
const shownName = computed(() =>
  cleanName(
    (nowConnected.value
      ? status.value.profile_name || shownProfile.value?.name
      : selected.value?.name) || "Выберите сервер",
  ),
);
const shownFlag = computed(() => {
  const profile = shownProfile.value;
  if (profile?.flag_image) return { kind: "image" as const, value: profile.flag_image };
  const emoji = profile ? flag(profile.name) : null;
  return emoji ? { kind: "emoji" as const, value: emoji } : null;
});
const profiles = computed(
  () =>
    snapshot.value?.profiles.filter(
      (p) => p.subscription_id === snapshot.value?.selected_subscription,
    ) ?? [],
);
const groups = computed(() => {
  const list =
    snapshot.value?.subscriptions.map((s) => ({
      id: s.id as string | null,
      name: s.name,
    })) ?? [];
  if (snapshot.value?.profiles.some((p) => !p.subscription_id))
    list.push({ id: null, name: "Мои серверы" });
  return list;
});
const activeIndex = computed(() =>
  groups.value.findIndex((s) => s.id === snapshot.value?.selected_subscription),
);
const rules = computed(
  () => snapshot.value?.settings.routing.applications ?? [],
);
const activeRules = computed(() => rules.value.filter((r) => r.bypass).length);
const filteredCatalog = computed(() => {
  const term = catalogSearch.value.toLocaleLowerCase();
  return catalog.value.filter((a) =>
    `${a.name} ${a.matcher}`.toLocaleLowerCase().includes(term),
  );
});
const filteredLogs = computed(() =>
  logs.value.filter(
    (l) =>
      (logLevel.value === "all" || l.level === logLevel.value) &&
      l.message
        .toLocaleLowerCase()
        .includes(logQuery.value.toLocaleLowerCase()),
  ),
);
const phaseLabel = computed(
  () =>
    busy.value === "disconnect" ? "Отключаемся" : ({
      connected: "VPN включён",
      connecting: "Подключаемся",
      disconnecting: "Отключаемся",
      disconnected: "VPN выключен",
      error: "Ошибка подключения",
    })[status.value.phase],
);
const dirty = computed(
  () =>
    draft.value &&
    JSON.stringify(draft.value) !== JSON.stringify(snapshot.value?.settings),
);
const reconnectMessage = () =>
  nowConnected.value
    ? "Правила сохранены. VPN перезапущен"
    : "Правила сохранены";
let unlisteners: UnlistenFn[] = [];
let nativeVisible = true, snapshotDirty = false;
const uiPoller = visibilityPoller(async () => {
  if (snapshotDirty) {
    await refresh();
    snapshotDirty = false;
  }
  const revision = status.value.logs_revision;
  await poll();
  if (page.value === "logs" && revision !== status.value.logs_revision)
    await loadLogs();
}, () => Math.max(1000, Number(snapshot.value?.settings.traffic_refresh_seconds ?? 1) * 1000));
function updateVisibility() {
  uiPoller.setVisible(nativeVisible && !document.hidden);
}
let catalogRevision = 0;

async function start() {
  loading.value = true;
  startupError.value = "";
  try {
    await Promise.all([refresh(), poll()]);
  } catch (e) {
    startupError.value = isTauri()
      ? String(e)
      : "Откройте NORY через Tauri. Браузерный просмотр не имеет доступа к VPN.";
  } finally {
    loading.value = false;
  }
}
onMounted(async () => {
  await start();
  if (isTauri()) {
    // The window remains hidden until the local state and first DOM are ready.
    // Do not wait for animation frames here: hidden WebViews can throttle them.
    await nextTick();
    try {
      await invoke("ui_ready");
    } catch {}
    try {
      maximized.value = await getCurrentWindow().isMaximized();
      unlisteners.push(
        await getCurrentWindow().onResized(async () => {
          try {
            maximized.value = await getCurrentWindow().isMaximized();
          } catch {}
        }),
      );
    } catch {}
    unlisteners.push(
      await listen<string>("operation-error", (e) => notice(e.payload, true)),
    );
    unlisteners.push(
      await listen("data-changed", () => {
        if (!nativeVisible || document.hidden) snapshotDirty = true;
        else void refresh().catch((e) => notice(String(e), true));
      }),
    );
    unlisteners.push(await listen<boolean>("window-visibility", (e) => {
      nativeVisible = e.payload;
      updateVisibility();
    }));
    unlisteners.push(
      await listen<{ received?: number; total?: number; verifying?: boolean }>(
        "update-progress",
        (e) => {
          updateProgress.value = e.payload.verifying
            ? "Проверка подписи…"
            : `${bytes(e.payload.received)} / ${bytes(e.payload.total)}`;
        },
      ),
    );
    nativeVisible = await getCurrentWindow().isVisible();
    document.addEventListener("visibilitychange", updateVisibility);
    updateVisibility();
    uiPoller.start();
  }
});
onBeforeUnmount(() => {
  document.removeEventListener("paste", pasteSubscription);
  uiPoller.stop();
  document.removeEventListener("visibilitychange", updateVisibility);
  unlisteners.forEach((f) => f());
});
watch(
  () => snapshot.value?.settings,
  (settings, previous) => {
    if (settings)
      draft.value = mergeSettingsDraft(settings, previous, draft.value);
  },
  { immediate: true },
);
watch(page, (p) => {
  document.querySelector("main")?.scrollTo({ top: 0 });
  if (p === "logs") void loadLogs();
});
watch(catalogMode, () => {
  if (catalogOpen.value) void loadCatalog();
});
async function connect() {
  const ok = await action(vpnRequested.value ? "disconnect" : "connect");
  if (
    ok &&
    nowConnected.value &&
    snapshot.value?.settings.minimize_on_connect
  ) {
    try {
      await getCurrentWindow().hide();
      nativeVisible = false;
      updateVisibility();
    } catch (e) {
      notice(String(e), true);
    }
  }
}
async function cycle(direction: number) {
  const g =
    groups.value[
      (activeIndex.value + direction + groups.value.length) %
        groups.value.length
    ];
  if (g) await action("select_subscription", { id: g.id });
}
async function refreshSub() {
  if (currentSub.value)
    await action(
      "refresh_subscription",
      { id: currentSub.value.id },
      "Подписка обновлена",
    );
}
async function openDetails(mode: "json" | "url") {
  const target = mode === "json" ? shownProfile.value : currentSub.value;
  if (!target) return;
  const revision = ++subscriptionDialogRevision;
  subscriptionDialog.value = mode;
  subscriptionDetails.value = { id: target.id, name: target.name };
  subscriptionDialogLoading.value = true;
  subscriptionDialogError.value = "";
  editedUrl.value = "";
  try {
    const result = await request<NonNullable<typeof subscriptionDetails.value>>({
      type: mode === "json" ? "profile_json" : "subscription_url", id: target.id,
    });
    if (revision !== subscriptionDialogRevision) return;
    subscriptionDetails.value = result;
    editedUrl.value = result.url ?? "";
  } catch (e) {
    if (revision === subscriptionDialogRevision) subscriptionDialogError.value = String(e);
  } finally {
    if (revision === subscriptionDialogRevision) {
      subscriptionDialogLoading.value = false;
      await nextTick();
      if (revision === subscriptionDialogRevision)
        document.querySelector<HTMLElement>("#edit-sub-url, #server-json")?.focus();
    }
  }
}
function closeSubscriptionDetails() {
  if (busy.value === "change_subscription_url") return;
  subscriptionDialogRevision++;
  subscriptionDialog.value = null;
  subscriptionDetails.value = null;
  editedUrl.value = "";
  subscriptionDialogError.value = "";
}
async function changeSubscriptionUrl() {
  const details = subscriptionDetails.value;
  if (!details) return;
  if (await action("change_subscription_url", { id: details.id, url: editedUrl.value },
    "Ссылка изменена. Подписка обновлена")) closeSubscriptionDetails();
}
async function addSubscription() {
  if (
    await action(
      importMode.value ? "import_links" : "add_subscription",
      importMode.value
        ? { text: subUrl.value }
        : { url: subUrl.value, send_hwid: sendHwid.value },
      importMode.value ? "Серверы добавлены" : "Подписка добавлена",
    )
  ) {
    adding.value = false;
    subUrl.value = "";
  }
}
function pasteSubscription(event: ClipboardEvent) {
  // Normal text-field paste must keep working. Clipboard access only happens
  // in a user-initiated paste event; no polling or permissions prompt.
  if (isEditing(event.target) || adding.value || catalogOpen.value || deleting.value || subscriptionDialog.value) return;
  const url = subscriptionFromPaste(event.clipboardData?.getData("text/plain") ?? "");
  if (!url) return;
  event.preventDefault();
  if (working.value || status.value.network_busy) {
    notice("Дождитесь завершения текущего действия", true);
    return;
  }
  void action("add_subscription", { url, send_hwid: sendHwid.value }, "Подписка добавлена");
}
onMounted(() => document.addEventListener("paste", pasteSubscription));
async function removeSubscription() {
  if (
    currentSub.value &&
    (await action(
      "delete_subscription",
      { id: currentSub.value.id },
      "Подписка удалена",
    ))
  )
    deleting.value = false;
}
async function saveSettings() {
  if (draft.value)
    await action(
      "save_settings",
      { settings: JSON.parse(JSON.stringify(draft.value)) },
      nowConnected.value
        ? "Настройки сохранены. VPN перезапущен"
        : "Настройки сохранены",
    );
}
async function changeMode(mode: string) {
  if (
    snapshot.value &&
    (mode === "tun" || mode === "mihomo_tun") &&
    mode !== snapshot.value.settings.mode
  )
    await action("save_settings", {
      settings: {
        ...snapshot.value.settings,
        mode,
      },
    });
}
async function ruBypass(enabled: boolean) {
  if (snapshot.value)
    await action(
      "save_settings",
      {
        settings: {
          ...snapshot.value.settings,
          routing: { ...snapshot.value.settings.routing, bypass_ru: enabled },
        },
      },
      reconnectMessage(),
    );
}
async function loadCatalog() {
  const revision = ++catalogRevision;
  catalogLoading.value = true;
  try {
    const result = await request<CatalogItem[]>({
      type: "catalog",
      processes: catalogMode.value === "processes",
    });
    if (revision === catalogRevision) catalog.value = result;
  } catch (e) {
    notice(String(e), true);
  } finally {
    if (revision === catalogRevision) catalogLoading.value = false;
  }
}
function showCatalog() {
  catalogOpen.value = true;
  catalogSearch.value = "";
  void loadCatalog();
}
async function addBypass(item: CatalogItem) {
  if (
    await action(
      "add_bypass",
      { name: item.name, matcher: item.matcher },
      reconnectMessage(),
    )
  )
    catalogOpen.value = false;
}
async function chooseFile() {
  try {
    const selected = await open({
      multiple: false,
      directory: false,
      filters:
        snapshot.value?.platform === "windows"
          ? [{ name: "Приложение", extensions: ["exe"] }]
          : [],
    });
    if (typeof selected === "string")
      await addBypass({
        name: selected.split(/[\\/]/).pop() ?? selected,
        matcher: selected,
      });
  } catch (e) {
    notice(String(e), true);
  }
}
async function loadLogs() {
  try {
    logs.value = await request<Log[]>({ type: "logs" });
  } catch (e) {
    notice(String(e), true);
  }
}
async function clearLogs() {
  try {
    await request({ type: "clear_logs" });
    await loadLogs();
    notice("Журнал очищен");
  } catch (e) {
    notice(String(e), true);
  }
}
async function checkUpdate() {
  updateBusy.value = true;
  try {
    update.value = await request({ type: "check_update" });
    if (!update.value) notice("Установлена актуальная версия");
  } catch (e) {
    notice(String(e), true);
  } finally {
    updateBusy.value = false;
  }
}
async function installUpdate() {
  updateBusy.value = true;
  updateProgress.value = "Загрузка…";
  try {
    await invoke("install_update");
  } catch (e) {
    notice(String(e), true);
  } finally {
    updateBusy.value = false;
  }
}
function setField(key: string, value: unknown) {
  if (draft.value) draft.value[key] = value;
}
function backdrop(event: MouseEvent, close: () => void) {
  if (event.target === event.currentTarget && !busy.value) close();
}
// Keep keyboard focus inside the visible dialog and return it to its trigger.
let restoreFocus: HTMLElement | null = null;
function modalKey(event: KeyboardEvent) {
  const modal = document.querySelector<HTMLElement>(".modal");
  if (!modal) return;
  if (event.key === "Escape" && !busy.value) {
    adding.value = false;
    deleting.value = false;
    catalogOpen.value = false;
    closeSubscriptionDetails();
    event.preventDefault();
  }
  if (event.key === "Tab") {
    const items = [
      ...modal.querySelectorAll<HTMLElement>(
        'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex="0"]',
      ),
    ];
    const first = items[0],
      last = items.at(-1);
    if (event.shiftKey && document.activeElement === first) {
      last?.focus();
      event.preventDefault();
    } else if (!event.shiftKey && document.activeElement === last) {
      first?.focus();
      event.preventDefault();
    }
  }
}
watch(
  () => adding.value || deleting.value || catalogOpen.value || !!subscriptionDialog.value,
  async (shown) => {
    if (shown) restoreFocus = document.activeElement as HTMLElement;
    await nextTick();
    document
      .querySelectorAll<HTMLElement>(".app-header, main, .bottom-nav")
      .forEach((el) => (el.inert = shown));
    if (shown) {
      document.addEventListener("keydown", modalKey);
      document
        .querySelector<HTMLElement>(
          ".modal [autofocus], .modal input, .modal button",
        )
        ?.focus();
    } else {
      document.removeEventListener("keydown", modalKey);
      restoreFocus?.focus();
    }
  },
);
onBeforeUnmount(() => document.removeEventListener("keydown", modalKey));
const categories = [
  { id: "connection", name: "Подключение" },
  { id: "network", name: "Сеть и DNS" },
  { id: "behavior", name: "Приложение" },
  { id: "advanced", name: "Дополнительно" },
];
const fields: Record<
  string,
  {
    key: string;
    label: string;
    description: string;
    type: "bool" | "number" | "text" | "select";
    min?: number;
    max?: number;
    options?: [string, string][];
  }[]
> = {
  connection: [
    {
      key: "mode",
      label: "Ядро VPN",
      description: "Один TUN-бэкенд в каждый момент времени",
      type: "select",
      options: [
        ["tun", "Xray + sing-box"],
        ["mihomo_tun", "Mihomo"],
      ],
    },
    {
      key: "tun_interface_name",
      label: "Имя интерфейса",
      description: "До 15 символов без пробелов",
      type: "text",
    },
    {
      key: "mtu",
      label: "MTU",
      description: "Размер сетевого пакета",
      type: "number",
      min: 1280,
      max: 9000,
    },
    {
      key: "tun_auto_route",
      label: "Автоматические маршруты",
      description: "Направлять системный трафик в TUN",
      type: "bool",
    },
    {
      key: "enable_ipv6",
      label: "IPv6",
      description: "Использовать IPv6 внутри VPN",
      type: "bool",
    },
    {
      key: "auto_connect",
      label: "Подключаться при запуске",
      description: "Использовать последний выбранный сервер",
      type: "bool",
    },
    {
      key: "auto_reconnect",
      label: "Переподключение",
      description: "Повторять подключение после сбоя ядра",
      type: "bool",
    },
    {
      key: "reconnect_delay_seconds",
      label: "Задержка повторения, с",
      description: "Пауза между попытками",
      type: "number",
      min: 1,
      max: 120,
    },
  ],
  network: [
    {
      key: "geosite_url",
      label: "URL GeoSite",
      description: "Для Xray: HTTPS-ссылка на .dat провайдера. Пусто — встроенная база RoscomVPN.",
      type: "text",
    },
    {
      key: "geoip_url",
      label: "URL GeoIP",
      description: "Для Xray: HTTPS-ссылка на .dat с IP-категориями. Пусто — встроенная база RoscomVPN.",
      type: "text",
    },
    {
      key: "dns_servers",
      label: "DNS-серверы",
      description: "Адреса через запятую; пусто — из конфигурации",
      type: "text",
    },
    {
      key: "domain_strategy",
      label: "Разрешение доменов",
      description: "Стратегия маршрутизации Xray",
      type: "select",
      options: [
        ["as_is", "AsIs"],
        ["ip_if_non_match", "IPIfNonMatch"],
        ["ip_on_demand", "IPOnDemand"],
      ],
    },
    {
      key: "sniffing",
      label: "Определение доменов",
      description: "Определять протокол и имя назначения",
      type: "bool",
    },
    {
      key: "sniffing_route_only",
      label: "Только для маршрутизации",
      description: "Не подменять адрес назначения",
      type: "bool",
    },
    {
      key: "ping_type",
      label: "Тип пинга",
      description: "Прокси проверяет HTTPS через выбранный сервер; ICMP и TCP — только узел/CDN. По 5 запросов вне активного VPN.",
      type: "select",
      options: [["proxy", "Прокси · HTTPS"], ["tcp", "TCP · порт сервера"], ["icmp", "ICMP · сетевой узел"]],
    },
    {
      key: "ping_url",
      label: "Адрес прокси-проверки",
      description: "По умолчанию — проверка подключения Google (gstatic), HTTP 204. Запрос идёт через проверяемый сервер. Можно указать свой HTTPS-адрес с ответом 204.",
      type: "text",
    },
    {
      key: "ping_timeout_seconds",
      label: "Тайм-аут запроса, с",
      description: "Пять запросов на каждый сервер",
      type: "number",
      min: 1,
      max: 10,
    },
    {
      key: "ping_parallelism",
      label: "Параллельных проверок",
      description: "Одновременно; для прокси-пинга максимум 2, чтобы не расходовать много памяти",
      type: "number",
      min: 1,
      max: 16,
    },
    {
      key: "auto_ping",
      label: "Пинг при запуске",
      description: "Проверять активную подписку",
      type: "bool",
    },
    {
      key: "auto_select_fastest",
      label: "Выбирать минимальный пинг",
      description: "После проверки, если VPN выключен",
      type: "bool",
    },
  ],
  behavior: [
    {
      key: "close_to_tray",
      label: "Закрывать в трей",
      description: "VPN продолжит работать в фоне",
      type: "bool",
    },
    {
      key: "start_minimized",
      label: "Запускать свёрнутым",
      description: "Окно можно открыть из трея",
      type: "bool",
    },
    {
      key: "start_at_login",
      label: "Автозапуск",
      description: "Открывать NORY при входе в систему",
      type: "bool",
    },
    {
      key: "minimize_on_connect",
      label: "Скрывать после подключения",
      description: "Управление остаётся в трее",
      type: "bool",
    },
    {
      key: "traffic_refresh_seconds",
      label: "Обновление статистики, с",
      description: "Интервал счётчиков на главном экране",
      type: "number",
      min: 1,
      max: 30,
    },
    {
      key: "auto_update_subscriptions",
      label: "Автообновление подписок",
      description: "Загружать изменения в фоне",
      type: "bool",
    },
    {
      key: "subscription_update_interval_hours",
      label: "Интервал обновлений, ч",
      description: "Для автоматического обновления подписок",
      type: "number",
      min: 1,
      max: 168,
    },
  ],
  advanced: [
    {
      key: "socks_port",
      label: "Внутренний SOCKS-порт",
      description: "Связь sing-box с Xray, только localhost",
      type: "number",
      min: 1024,
      max: 65535,
    },
    {
      key: "api_port",
      label: "Порт API Xray",
      description: "Локальный API ядра",
      type: "number",
      min: 1024,
      max: 65535,
    },
    {
      key: "mux_enabled",
      label: "Мультиплексирование",
      description: "Зависит от протокола сервера",
      type: "bool",
    },
    {
      key: "mux_concurrency",
      label: "Потоков Mux",
      description: "Максимальное количество потоков",
      type: "number",
      min: 1,
      max: 128,
    },
    {
      key: "tls_allow_insecure",
      label: "Пропуск проверки TLS",
      description: "Небезопасно. Включайте только для диагностики",
      type: "bool",
    },
    {
      key: "log_level",
      label: "Уровень логов",
      description: "Подробность сообщений ядра",
      type: "select",
      options: [
        ["none", "Отключены"],
        ["error", "Ошибки"],
        ["warning", "Предупреждения"],
        ["info", "Информация"],
      ],
    },
  ],
};
</script>

<template>
  <div class="app-shell">
    <header class="app-header" data-tauri-drag-region>
      <a
        class="brand"
        href="#"
        aria-label="Главная NORY"
        @click.prevent="page = 'home'"
        ><img :src="logo" alt="" /><span>NORY</span
        ><span class="version" v-if="snapshot">{{ snapshot.version }}</span></a
      >
      <div class="window-drag-space" data-tauri-drag-region />
      <button class="theme-toggle" type="button"
        :aria-label="theme === 'dark' ? 'Включить светлую тему' : 'Включить тёмную тему'"
        :title="theme === 'dark' ? 'Светлая тема' : 'Тёмная тема'" @click="toggleTheme">
        <Sun v-if="theme === 'dark'" :size="17" /><Moon v-else :size="17" />
      </button>
      <div class="window-controls" aria-label="Управление окном">
        <button
          class="window-control"
          aria-label="Свернуть окно"
          title="Свернуть"
          @click="windowAction('minimize')"
        >
          <Minus :size="16" />
        </button>
        <button
          class="window-control"
          :aria-label="maximized ? 'Восстановить окно' : 'Развернуть окно'"
          :title="maximized ? 'Восстановить' : 'Развернуть'"
          @click="windowAction('toggleMaximize')"
        >
          <Copy v-if="maximized" :size="13" /><Square v-else :size="13" />
        </button>
        <button
          class="window-control close-window"
          aria-label="Закрыть окно"
          title="Закрыть"
          @click="windowAction('close')"
        >
          <X :size="17" />
        </button>
      </div>
    </header>
    <main>
      <div v-if="loading" class="startup">
        <LoaderCircle class="spin" :size="26" />
        <p>Загружаем NORY</p>
      </div>
      <div v-else-if="startupError" class="empty card">
        <AlertCircle :size="28" />
        <h2>Не удалось открыть данные</h2>
        <p>{{ startupError }}</p>
        <button class="button primary" @click="start">Повторить</button>
      </div>
      <template v-else-if="snapshot">
        <section v-if="page === 'home'" class="page home-page">
          <div class="page-heading">
            <div>
              <span class="page-kicker">ВАШЕ ПРОСТРАНСТВО В СЕТИ</span>
              <h1>Подключение</h1>
            </div>
            <div class="core-select">
              <SlidersHorizontal :size="15" />
              <SelectMenu
                :model-value="snapshot.settings.mode"
                :disabled="working"
                label="Выбор ядра"
                :options="[
                  ['tun', 'Xray + sing-box'],
                  ['mihomo_tun', 'Mihomo'],
                ]"
                @update:model-value="changeMode"
              />
            </div>
          </div>
          <div
            class="connection-grid mb-6 grid grid-cols-[180px_minmax(0,1fr)] gap-3 min-[900px]:grid-cols-[212px_minmax(0,1fr)]"
            :class="{ online: nowConnected }"
          >
            <WorldMap />
            <section
              class="card connection-card"
              :class="{ online: nowConnected }"
            >
              <button
                class="power-button"
                :class="{ online: nowConnected }"
                :aria-label="vpnRequested ? 'Отключить VPN' : 'Включить VPN'"
                :disabled="(working && !canCancelConnecting) || (!selected && !vpnRequested)"
                @click="connect"
              >
                <LoaderCircle v-if="working" :size="33" class="spin" /><Power
                  v-else
                  :size="33"
                  :stroke-width="1.6"
                /></button
              ><strong>{{ phaseLabel }}</strong
              ><span class="timer">{{ duration(status.seconds) }}</span>
            </section>
            <div class="connection-detail">
              <section class="card selected-card">
                <div class="selected-heading">
                  <span class="eyebrow">{{
                    nowConnected ? "Текущий сервер" : "Выбранный сервер"
                  }}</span>
                  <span v-if="shownProfile" class="selected-latency" :title="`Среднее время ответа · ${snapshot.settings.ping_type ?? 'icmp'} · 5 запросов`">
                    <Zap :size="13" />
                    {{ shownProfile.latency_ms === null ? "n/a" : `${shownProfile.latency_ms} мс` }}
                  </span>
                </div>
                <div class="selected-title">
                  <img
                    v-if="shownFlag?.kind === 'image'"
                    :src="shownFlag.value"
                    class="selected-flag"
                    alt=""
                  />
                  <span v-else-if="shownFlag?.kind === 'emoji'" class="selected-flag emoji">{{ shownFlag.value }}</span>
                  <Globe2 v-else :size="21" />
                  <h2>
                    <button v-if="shownProfile" class="server-json-trigger"
                      :aria-label="`Открыть JSON сервера: ${shownName}`" title="Посмотреть JSON сервера"
                      @click="openDetails('json')">
                      <span>{{ shownName }}</span><Braces :size="17" aria-hidden="true" />
                    </button>
                    <template v-else>{{ shownName }}</template>
                  </h2>
                </div>
                <p>
                  {{
                    status.phase === "error"
                      ? status.error
                      : nowConnected
                        ? "Соединение работает. Управляйте VPN здесь или из трея."
                        : "Выберите сервер и включите VPN"
                  }}
                </p>
                <p v-if="shownProfile?.description" class="server-description">{{ shownProfile.description }}</p>
                <div class="selected-protocols" v-if="shownProfile">
                  <ProtocolBadges :profile="shownProfile" />
                </div>
              </section>
              <div class="traffic-grid card">
                <section class="traffic-card">
                  <span><ArrowDownLeft :size="16" />Получено</span
                  ><strong>{{ bytes(status.download) }}</strong>
                </section>
                <section class="traffic-card">
                  <span><ArrowUpRight :size="16" />Отправлено</span
                  ><strong>{{ bytes(status.upload) }}</strong>
                </section>
              </div>
            </div>
          </div>
          <section class="subscription-area">
            <div class="subscription-bar card">
              <button
                v-if="groups.length > 1"
                class="icon-button plain"
                aria-label="Предыдущая подписка"
                :disabled="working"
                @click="cycle(-1)"
              >
                <ChevronLeft :size="19" /></button
              ><button
                class="subscription-toggle"
                :aria-expanded="expanded"
                @click="expanded = !expanded"
              >
                <Globe2 :size="19" /><span
                  ><strong>{{ currentSub?.name ?? "Мои серверы" }}</strong
                  ><small
                    >{{ profiles.length }} серверов<template v-if="currentSub">
                      ·
                      {{
                        bytes(
                          (currentSub.upload_bytes ?? 0) +
                            (currentSub.download_bytes ?? 0),
                        )
                      }}
                      /
                      {{
                        currentSub.total_bytes
                          ? bytes(currentSub.total_bytes)
                          : "∞"
                      }}<template v-if="currentSub.expires_at"> · до {{ date(currentSub.expires_at) }}</template></template
                    ></small
                  ></span
                ><ChevronDown
                  :size="16"
                  :class="{ 'rotate-180': expanded }"
                /></button
              ><button
                v-if="groups.length > 1"
                class="icon-button plain"
                aria-label="Следующая подписка"
                :disabled="working"
                @click="cycle(1)"
              >
                <ChevronRight :size="19" />
              </button>
              <div class="subscription-tools">
                <button
                  class="icon-button circle"
                  :title="`Пинг этой подписки · ${snapshot.settings.ping_type ?? 'icmp'} · 5 запросов вне активного VPN`"
                  aria-label="Пинг этой подписки"
                  :disabled="working || !profiles.length || status.network_busy"
                  @click="
                    action(
                      'ping',
                      { subscription_id: snapshot.selected_subscription },
                      `Проверка подписки «${currentSub?.name ?? 'Мои серверы'}» завершена`,
                    )
                  "
                >
                  <LoaderCircle
                    v-if="busy === 'ping'"
                    :size="17"
                    class="spin"
                  /><Zap v-else :size="17" /></button
                ><button
                  v-if="currentSub"
                  class="icon-button circle"
                  title="Обновить подписку"
                  aria-label="Обновить подписку"
                  :disabled="working || status.network_busy"
                  @click="refreshSub"
                >
                  <RefreshCw
                    :size="16"
                    :class="{ spin: busy === 'refresh_subscription' }"
                  /></button>
                <button v-if="currentSub" class="icon-button circle"
                  title="Изменить ссылку подписки" aria-label="Изменить ссылку подписки"
                  :disabled="working || status.network_busy" @click="openDetails('url')">
                  <Link2 :size="16" />
                </button
                ><button
                  v-if="currentSub"
                  class="icon-button circle danger"
                  title="Удалить подписку"
                  aria-label="Удалить подписку"
                  :disabled="working"
                  @click="deleting = true"
                >
                  <Trash2 :size="16" /></button
                ><button class="button small" aria-label="Подписка" title="Добавить подписку · Ctrl+V" @click="adding = true">
                  <Plus :size="16" /><span>Подписка</span>
                </button>
              </div>
            </div>
            <aside v-if="currentSub?.description" class="subscription-note">
              <strong>Сообщение от провайдера</strong>
              <p>{{ currentSub.description }}</p>
            </aside>
            <div v-if="expanded">
              <div
                v-if="profiles.length"
                class="server-grid mt-4 grid grid-cols-2 gap-3 min-[900px]:grid-cols-3"
              >
                <ServerCard
                  v-for="profile in profiles"
                  v-memo="[profile, profile.id === snapshot.selected_profile, working]"
                  :key="profile.id"
                  :profile="profile"
                  :selected="profile.id === snapshot.selected_profile"
                  :disabled="working"
                  @select="action('select_profile', { id: profile.id })"
                />
              </div>
              <div v-else class="empty card">
                <Globe2 :size="30" />
                <h2>Добавьте подписку</h2>
                <p>
                  Добавьте ссылку от провайдера или импортируйте конфигурацию.
                </p>
                <button class="button primary" @click="adding = true">
                  <Plus :size="16" />Добавить подписку
                </button>
              </div>
              <p v-if="currentSub" class="sub-updated">
                Обновлено: {{ date(currentSub.updated_at) }}
                <span v-if="currentSub.expires_at">
                  · До {{ date(currentSub.expires_at) }}</span
                >
              </p>
            </div>
          </section>
        </section>

        <section v-if="page === 'bypass'" class="page">
          <div class="page-heading">
            <div>
              <h1>В обход VPN</h1>
              <p>Выбранные приложения подключаются напрямую.</p>
            </div>
            <button class="button primary" @click="showCatalog">
              <Plus :size="16" />Приложение
            </button>
          </div>
          <section class="card setting-row ru-card">
            <div class="setting-symbol">RU</div>
            <div class="setting-copy">
              <strong>Российские сайты и IP</strong>
              <p>Встроенные GeoData RU · применяется к обоим ядрам</p>
            </div>
            <Toggle
              :model-value="snapshot.settings.routing.bypass_ru"
              label="Обход GeoData RU"
              :disabled="working"
              @update:model-value="ruBypass"
            />
          </section>
          <div class="section-caption">
            <span>Приложения</span
            ><span>{{ activeRules }} активных правил</span>
          </div>
          <div v-if="rules.length" class="rule-grid">
            <section
              v-for="rule in rules"
              :key="rule.id"
              class="card rule-card"
            >
              <div class="flex gap-3 items-start min-w-0">
                <Monitor :size="20" class="shrink-0" />
                <div class="min-w-0">
                  <strong class="block truncate">{{ rule.name }}</strong>
                  <p class="path" :title="rule.processes.join(', ')">
                    {{ rule.processes.join(", ") }}
                  </p>
                </div>
              </div>
              <div class="flex items-center justify-between mt-5">
                <button
                  class="icon-button danger"
                  title="Удалить правило"
                  aria-label="Удалить правило"
                  :disabled="working"
                  @click="
                    action('delete_bypass', { id: rule.id }, reconnectMessage())
                  "
                >
                  <Trash2 :size="15" /></button
                ><Toggle
                  :model-value="rule.bypass"
                  :label="`Обход для ${rule.name}`"
                  :disabled="working"
                  @update:model-value="
                    (enabled) =>
                      action(
                        'toggle_bypass',
                        { id: rule.id, enabled },
                        reconnectMessage(),
                      )
                  "
                />
              </div>
            </section>
          </div>
          <div v-else class="empty card">
            <Route :size="30" />
            <h2>Нет правил обхода</h2>
            <p>Выберите приложение из списка, запущенный процесс или файл.</p>
            <div class="flex flex-wrap gap-2 justify-center">
              <button class="button" @click="showCatalog">
                <Monitor :size="16" />Из списка</button
              ><button class="button" @click="chooseFile">
                <FolderOpen :size="16" />Из папки
              </button>
            </div>
          </div>
          <p class="footnote">
            При изменении правил активное соединение перезапустится. Уже
            открытые соединения приложения может потребоваться открыть заново.
          </p>
        </section>

        <section v-if="page === 'logs'" class="page">
          <div class="page-heading">
            <div>
              <h1>Журнал событий</h1>
              <p>Сообщения ядра для поиска причин ошибки.</p>
            </div>
            <button class="button" @click="clearLogs">
              <Trash2 :size="15" />Очистить
            </button>
          </div>
          <div class="log-toolbar">
            <label class="search-field"
              ><Search :size="17" /><input
                v-model="logQuery"
                placeholder="Поиск по журналу"
                aria-label="Поиск по журналу" /></label
            ><SelectMenu
              v-model="logLevel"
              label="Уровень сообщений"
              :options="[
                ['all', 'Все уровни'],
                ['error', 'Ошибки'],
                ['warning', 'Предупреждения'],
                ['info', 'Информация'],
              ]"
            /><button
              class="icon-button"
              aria-label="Обновить журнал"
              @click="loadLogs"
            >
              <RefreshCw :size="17" />
            </button>
          </div>
          <div class="card log-panel">
            <div v-if="!filteredLogs.length" class="empty">
              <ScrollText :size="28" />
              <p>Здесь появятся сообщения после подключения.</p>
            </div>
            <div
              v-for="(log, index) in filteredLogs"
              :key="`${log.at}-${index}`"
              class="log-line"
              :class="log.level"
            >
              <time>{{ new Date(log.at * 1000).toLocaleTimeString("ru") }}</time
              ><span class="log-level">{{ log.level }}</span>
              <p>{{ log.message }}</p>
            </div>
          </div>
          <p class="footnote">
            Перед отправкой логов проверьте, что в них нет паролей, UUID и
            ссылок подписки.
          </p>
        </section>

        <section v-if="page === 'settings'" class="page">
          <div class="page-heading">
            <div>
              <h1>Настройки</h1>
              <p>Подключение, сеть и поведение приложения.</p>
            </div>
            <button
              class="button primary"
              :disabled="!dirty || working"
              @click="saveSettings"
            >
              <LoaderCircle
                v-if="busy === 'save_settings'"
                :size="16"
                class="spin"
              /><Check v-else :size="16" />Сохранить
            </button>
          </div>
          <div class="settings-layout">
            <aside class="settings-nav">
              <button
                v-for="category in categories"
                :key="category.id"
                :class="{ active: settingsCategory === category.id }"
                @click="settingsCategory = category.id"
              >
                {{ category.name }}<ChevronRight :size="14" />
              </button>
            </aside>
            <div class="settings-content" v-if="draft">
              <section v-if="settingsCategory === 'behavior'" class="card appearance-card">
                <div><span class="page-kicker">ОФОРМЛЕНИЕ</span><h2>В своём свете</h2><p>Выберите тему. Она сохранится автоматически.</p></div>
                <div class="appearance-options" role="group" aria-label="Цветовая тема">
                  <button type="button" class="appearance-option light-preview" :class="{ chosen: theme === 'light' }" :aria-pressed="theme === 'light'" @click="setTheme('light')">
                    <span class="theme-preview" aria-hidden="true"><i /><i /><i /></span>
                    <span><Sun :size="16" />Светлая<Check v-if="theme === 'light'" :size="15" /></span>
                  </button>
                  <button type="button" class="appearance-option dark-preview" :class="{ chosen: theme === 'dark' }" :aria-pressed="theme === 'dark'" @click="setTheme('dark')">
                    <span class="theme-preview" aria-hidden="true"><i /><i /><i /></span>
                    <span><Moon :size="16" />Тёмная<Check v-if="theme === 'dark'" :size="15" /></span>
                  </button>
                </div>
              </section>
              <section class="card settings-card">
                <div
                  v-for="field in fields[settingsCategory]"
                  :key="field.key"
                  class="setting-row"
                >
                  <div class="setting-copy">
                    <label :for="`field-${field.key}`">{{ field.label }}</label>
                    <p>{{ field.description }}</p>
                  </div>
                  <Toggle
                    v-if="field.type === 'bool'"
                    :model-value="Boolean(draft[field.key])"
                    :label="field.label"
                    @update:model-value="(value) => setField(field.key, value)"
                  /><SelectMenu
                    v-else-if="field.type === 'select'"
                    :id="`field-${field.key}`"
                    :model-value="String(draft[field.key])"
                    :label="field.label"
                    :options="field.options ?? []"
                    @update:model-value="setField(field.key, $event)"
                  /><input
                    v-else
                    :id="`field-${field.key}`"
                    :type="field.type"
                    :value="draft[field.key]"
                    :min="field.min"
                    :max="field.max"
                    :class="{ 'number-input': field.type === 'number' }"
                    @input="
                      setField(
                        field.key,
                        field.type === 'number'
                          ? Number(($event.target as HTMLInputElement).value)
                          : ($event.target as HTMLInputElement).value,
                      )
                    "
                  />
                </div>
              </section>
              <section class="card update-card">
                <img :src="logo" alt="" />
                <div class="min-w-0">
                  <strong>NORY {{ snapshot.version }}</strong>
                  <p>
                    {{
                      snapshot.platform === "windows"
                        ? "Для Windows"
                        : "Для Linux"
                    }}

                  </p>
                </div>
                <button
                  class="button ml-auto"
                  :disabled="updateBusy"
                  @click="checkUpdate"
                >
                  <RefreshCw
                    :size="15"
                    :class="{ spin: updateBusy }"
                  />Проверить обновления
                </button>
              </section>
              <section v-if="update" class="card p-5">
                <h3>Доступна {{ update.version }}</h3>
                <p class="muted mt-2">{{ update.notes }}</p>
                <button
                  class="button primary mt-4"
                  :disabled="updateBusy"
                  @click="installUpdate"
                >
                  <Download :size="16" />{{
                    updateBusy ? updateProgress : "Скачать и установить"
                  }}
                </button>
              </section>
            </div>
          </div>
        </section>
        <footer>
          <button
            @click="
              invoke('developer_channel').catch((e) => notice(String(e), true))
            "
          >
            TG канал разработчика <ExternalLink :size="12" />
          </button>
        </footer>
      </template>
    </main>

    <nav class="bottom-nav" aria-label="Основная навигация">
      <button
        v-for="item in [
          { id: 'home', label: 'Подключение', icon: Globe2 },
          { id: 'bypass', label: 'Обход', icon: Route },
          { id: 'logs', label: 'Логи', icon: ScrollText },
          { id: 'settings', label: 'Настройки', icon: Settings2 },
        ] as const"
        :key="item.id"
        :class="{ active: page === item.id }"
        :aria-current="page === item.id ? 'page' : undefined"
        @click="page = item.id"
      >
        <component :is="item.icon" :size="18" :stroke-width="1.65" /><span>{{
          item.label
        }}</span>
      </button>
    </nav>
    <div class="notices" aria-live="polite">
      <div
        v-for="item in notices"
        :key="item.id"
        class="notice"
        :class="{ error: item.error }"
      >
        <AlertCircle v-if="item.error" :size="17" class="shrink-0" />
        <p>{{ item.text }}</p>
        <button
          v-if="item.retry"
          class="text-button"
          :disabled="working"
          @click="item.retry"
        >
          Повторить</button
        ><button
          class="icon-button"
          aria-label="Закрыть уведомление"
          @click="dismiss(item.id)"
        >
          <X :size="16" />
        </button>
      </div>
    </div>

    <div
      v-if="adding"
      class="modal-backdrop"
      @click="backdrop($event, () => (adding = false))"
      @keydown.esc="adding = false"
    >
      <section
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="add-title"
      >
        <div class="modal-heading">
          <div>
            <h2 id="add-title">
              {{ importMode ? "Импорт серверов" : "Добавить подписку" }}
            </h2>
            <p class="muted">Ctrl+V на главном экране · User-Agent: NORY/{{ snapshot?.version }}</p>
          </div>
          <button
            class="icon-button"
            aria-label="Закрыть"
            :disabled="working"
            @click="adding = false"
          >
            <X :size="19" />
          </button>
        </div>
        <div class="segmented">
          <button
            :class="{ active: !importMode }"
            :disabled="working"
            @click="importMode = false"
          >
            Ссылка подписки</button
          ><button
            :class="{ active: importMode }"
            :disabled="working"
            @click="importMode = true"
          >
            Ссылки / JSON
          </button>
        </div>
        <form @submit.prevent="addSubscription">
          <label for="sub-url">{{
            importMode ? "Конфигурация" : "Ссылка от провайдера"
          }}</label
          ><textarea
            v-if="importMode"
            id="sub-url"
            v-model="subUrl"
            rows="6"
            placeholder="vless://… или JSON-конфигурация"
            required
            autofocus
          /><input
            v-else
            id="sub-url"
            v-model="subUrl"
            type="text"
            placeholder="https://…"
            autocomplete="off"
            spellcheck="false"
            required
            autofocus
          />
          <div v-if="!importMode" class="setting-row">
            <div class="setting-copy">
              <strong>Отправлять HWID</strong>
              <p>Постоянный идентификатор этого устройства</p>
            </div>
            <Toggle v-model="sendHwid" label="Отправлять HWID" />
          </div>
          <p class="footnote">
            Название и описания серверов загрузятся автоматически. Ссылка
            хранится только на этом устройстве.
          </p>
          <button
            class="button primary w-full mt-5"
            :disabled="working || !subUrl.trim()"
          >
            <LoaderCircle v-if="working" class="spin" :size="16" /><Plus
              v-else
              :size="16"
            />{{ working ? "Загружаем…" : "Добавить" }}
          </button>
        </form>
      </section>
    </div>

    <div v-if="subscriptionDialog" class="modal-backdrop"
      @click="backdrop($event, closeSubscriptionDetails)">
      <section class="modal" :class="{ 'json-modal': subscriptionDialog === 'json' }"
        role="dialog" aria-modal="true" aria-labelledby="subscription-details-title">
        <div class="modal-heading">
          <div>
            <h2 id="subscription-details-title">{{ subscriptionDialog === 'json' ? 'JSON сервера' : 'Изменить ссылку' }}</h2>
            <p class="muted">{{ subscriptionDetails?.name }}</p>
          </div>
          <button class="icon-button" aria-label="Закрыть" :disabled="busy === 'change_subscription_url'"
            @click="closeSubscriptionDetails"><X :size="19" /></button>
        </div>
        <p v-if="subscriptionDialogLoading" class="muted" role="status">Загружаем…</p>
        <p v-else-if="subscriptionDialogError" role="alert">{{ subscriptionDialogError }}</p>
        <template v-else-if="subscriptionDialog === 'json'">
          <p id="json-privacy" class="footnote mb-3">
            {{ subscriptionDetails?.original ? 'Сохранённый JSON только этого сервера, включая его балансировщик, если он есть.' : 'JSON Xray сформирован из ссылки этого сервера. Это не исходный ответ провайдера.' }}
            Здесь могут быть пароли и ключи. Не публикуйте этот текст.
          </p>
          <textarea id="server-json" class="json-content" :value="subscriptionDetails?.text"
            readonly spellcheck="false" wrap="off" aria-label="Содержимое JSON сервера"
            aria-describedby="json-privacy" />
          <p class="footnote">Только просмотр · Ctrl+A и Ctrl+C для копирования</p>
        </template>
        <form v-else @submit.prevent="changeSubscriptionUrl">
          <label for="edit-sub-url">Новая ссылка от провайдера</label>
          <input id="edit-sub-url" v-model="editedUrl" type="url" required autocomplete="off"
            spellcheck="false" :disabled="working" />
          <p class="footnote">Сначала проверим новую ссылку. Если загрузка не получится, старая подписка останется без изменений. HWID и положение подписки сохранятся.</p>
          <p v-if="vpnRequested" class="footnote">Текущее VPN-соединение не будет прервано; новый список используется при следующем подключении.</p>
          <button class="button primary w-full mt-5" :disabled="working || !editedUrl.trim() || editedUrl.trim() === subscriptionDetails?.url">
            <LoaderCircle v-if="working" class="spin" :size="16" /><Check v-else :size="16" />
            {{ working ? 'Проверяем ссылку…' : 'Сохранить и обновить' }}
          </button>
        </form>
      </section>
    </div>

    <div
      v-if="deleting"
      class="modal-backdrop"
      @click="backdrop($event, () => (deleting = false))"
    >
      <section
        class="modal narrow"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="delete-title"
      >
        <Trash2 :size="25" class="text-red-300 mb-4" />
        <h2 id="delete-title">Удалить подписку?</h2>
        <p class="muted mt-3">
          «{{ currentSub?.name }}» и её серверы будут удалены. Если VPN
          использует эту подписку, он отключится.
        </p>
        <div class="flex justify-end gap-2 mt-6">
          <button class="button" :disabled="working" @click="deleting = false">
            Отмена</button
          ><button
            class="button destructive"
            :disabled="working"
            @click="removeSubscription"
          >
            Удалить
          </button>
        </div>
      </section>
    </div>

    <div
      v-if="catalogOpen"
      class="modal-backdrop"
      @click="backdrop($event, () => (catalogOpen = false))"
      @keydown.esc="catalogOpen = false"
    >
      <section
        class="modal catalog-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="catalog-title"
      >
        <div class="modal-heading">
          <div>
            <h2 id="catalog-title">Выберите приложение</h2>
          </div>
          <button
            class="icon-button"
            aria-label="Закрыть"
            @click="catalogOpen = false"
          >
            <X :size="19" />
          </button>
        </div>
        <div class="segmented">
          <button
            :class="{ active: catalogMode === 'apps' }"
            @click="catalogMode = 'apps'"
          >
            <Monitor :size="15" />Приложения</button
          ><button
            :class="{ active: catalogMode === 'processes' }"
            @click="catalogMode = 'processes'"
          >
            <Activity :size="15" />Процессы
          </button>
        </div>
        <div class="flex gap-2 mb-3">
          <label class="search-field flex-1"
            ><Search :size="17" /><input
              v-model="catalogSearch"
              placeholder="Название или путь"
              aria-label="Поиск приложения" /></label
          ><button
            class="icon-button"
            :disabled="catalogLoading"
            aria-label="Обновить список"
            @click="loadCatalog"
          >
            <RefreshCw :size="17" :class="{ spin: catalogLoading }" />
          </button>
        </div>
        <div class="catalog-list">
          <div v-if="catalogLoading" class="empty">
            <LoaderCircle class="spin" />
            <p>Ищем приложения…</p>
          </div>
          <div v-else-if="!filteredCatalog.length" class="empty">
            <Search :size="24" />
            <p>Ничего не найдено. Попробуйте выбрать файл.</p>
          </div>
          <button
            v-for="item in filteredCatalog"
            v-else
            :key="item.matcher"
            class="catalog-item"
            :disabled="
              working || rules.some((r) => r.processes.includes(item.matcher))
            "
            @click="addBypass(item)"
          >
            <Monitor :size="18" /><span
              ><strong>{{ item.name }}</strong
              ><small>{{ item.matcher }}</small></span
            ><Check
              v-if="rules.some((r) => r.processes.includes(item.matcher))"
              :size="16"
            /><Plus v-else :size="16" />
          </button>
        </div>
        <div class="modal-footer">
          <small>{{ filteredCatalog.length }} найдено</small
          ><button class="button" @click="chooseFile">
            <FolderOpen :size="16" />Выбрать файл
          </button>
        </div>
      </section>
    </div>
  </div>
</template>
