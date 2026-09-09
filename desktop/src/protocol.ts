import type { Profile } from "./types";

export function protocolBadges(profile: Pick<Profile, "protocol" | "transport" | "security" | "format">) {
  const network = profile.transport.toUpperCase();
  const badges = [
    { kind: "protocol", label: profile.protocol },
    { kind: "transport", label: network === "RAW" ? "TCP" : network === "WEBSOCKET" ? "WS" : network },
  ];
  if (profile.format === "json") badges.push({ kind: "format", label: "JSON" });
  if (["REALITY", "TLS"].includes(profile.security.toUpperCase()))
    badges.push({ kind: "security", label: profile.security.toUpperCase() });
  return badges.filter((badge) => badge.label);
}
