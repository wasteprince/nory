export interface Profile {
  id: string;
  name: string;
  description: string | null;
  protocol: string;
  transport: string;
  security: string;
  format: string;
  subscription_id: string | null;
  latency_ms: number | null;
  favorite: boolean;
  flag_image: string | null;
}
export interface Subscription {
  id: string;
  name: string;
  description: string | null;
  updated_at: number | null;
  expires_at: number | null;
  upload_bytes: number | null;
  download_bytes: number | null;
  total_bytes: number | null;
  send_hwid: boolean;
}
export interface Rule {
  id: string;
  name: string;
  processes: string[];
  source: string;
  bypass: boolean;
}
export interface Settings {
  mode: "tun" | "mihomo_tun";
  routing: {
    bypass_ru: boolean;
    applications: Rule[];
    bypass_domains: string[];
    bypass_geodata: unknown[];
  };
  [key: string]: unknown;
}
export interface Snapshot {
  version: string;
  platform: string;
  profiles: Profile[];
  subscriptions: Subscription[];
  settings: Settings;
  selected_profile: string | null;
  selected_subscription: string | null;
}
export interface Status {
  phase:
    "disconnected" | "connecting" | "connected" | "disconnecting" | "error";
  error: string | null;
  seconds: number;
  upload: number;
  download: number;
  profile_id: string | null;
  profile_name: string | null;
  logs_revision: number;
  network_busy: boolean;
  vpn_requested: boolean;
}
export interface CatalogItem {
  name: string;
  matcher: string;
}
export interface Log {
  at: number;
  level: string;
  message: string;
}
export interface Notice {
  id: number;
  text: string;
  error: boolean;
  retry?: () => void;
}
