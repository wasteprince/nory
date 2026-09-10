use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

pub const STATE_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppData {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub selected_subscription: Option<Uuid>,
    pub selected_profile: Option<Uuid>,
}

fn state_version() -> u32 {
    STATE_VERSION
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            settings: Settings::default(),
            profiles: Vec::new(),
            subscriptions: Vec::new(),
            selected_subscription: None,
            selected_profile: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub mode: ConnectionMode,
    #[serde(default = "default_socks_port")]
    pub socks_port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default)]
    pub allow_lan: bool,
    #[serde(default)]
    pub enable_ipv6: bool,
    #[serde(default = "default_true")]
    pub sniffing: bool,
    #[serde(default = "default_true")]
    pub sniffing_route_only: bool,
    #[serde(default = "default_true")]
    pub socks_udp: bool,
    #[serde(default)]
    pub dns_servers: String,
    #[serde(default)]
    pub geosite_url: String,
    #[serde(default)]
    pub geoip_url: String,
    #[serde(default)]
    pub domain_strategy: DomainStrategy,
    #[serde(default)]
    pub mux_enabled: bool,
    #[serde(default = "default_mux_concurrency")]
    pub mux_concurrency: u16,
    #[serde(default)]
    pub tls_allow_insecure: bool,
    #[serde(default = "default_tun_name")]
    pub tun_interface_name: String,
    #[serde(default = "default_true")]
    pub tun_auto_route: bool,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub auto_ping: bool,
    #[serde(default)]
    pub auto_select_fastest: bool,
    #[serde(default = "default_ping_timeout")]
    pub ping_timeout_seconds: u8,
    #[serde(default = "default_ping_parallelism")]
    pub ping_parallelism: u8,
    #[serde(default)]
    pub ping_type: PingType,
    #[serde(default = "default_ping_url")]
    pub ping_url: String,
    #[serde(default = "default_traffic_refresh")]
    pub traffic_refresh_seconds: u8,
    #[serde(default)]
    pub auto_reconnect: bool,
    #[serde(default = "default_reconnect_delay")]
    pub reconnect_delay_seconds: u8,
    #[serde(default = "default_true")]
    pub auto_update_subscriptions: bool,
    #[serde(default = "default_subscription_update_hours")]
    pub subscription_update_interval_hours: u16,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub minimize_on_connect: bool,
    #[serde(default)]
    pub start_at_login: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default)]
    pub routing: RoutingSettings,
    #[serde(default = "default_update_hours")]
    pub core_update_interval_hours: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::Tun,
            socks_port: default_socks_port(),
            http_port: default_http_port(),
            api_port: default_api_port(),
            allow_lan: false,
            enable_ipv6: false,
            sniffing: true,
            sniffing_route_only: true,
            socks_udp: true,
            dns_servers: String::new(),
            geosite_url: String::new(),
            geoip_url: String::new(),
            domain_strategy: DomainStrategy::default(),
            mux_enabled: false,
            mux_concurrency: default_mux_concurrency(),
            tls_allow_insecure: false,
            tun_interface_name: default_tun_name(),
            tun_auto_route: true,
            auto_connect: false,
            auto_ping: false,
            auto_select_fastest: false,
            ping_timeout_seconds: default_ping_timeout(),
            ping_parallelism: default_ping_parallelism(),
            ping_type: PingType::default(),
            ping_url: default_ping_url(),
            traffic_refresh_seconds: default_traffic_refresh(),
            auto_reconnect: false,
            reconnect_delay_seconds: default_reconnect_delay(),
            auto_update_subscriptions: true,
            subscription_update_interval_hours: default_subscription_update_hours(),
            start_minimized: false,
            minimize_on_connect: false,
            start_at_login: false,
            close_to_tray: true,
            mtu: default_mtu(),
            log_level: LogLevel::Warning,
            routing: RoutingSettings::default(),
            core_update_interval_hours: default_update_hours(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PingType {
    Proxy,
    Tcp,
    #[default]
    Icmp,
}

pub(crate) fn default_ping_url() -> String {
    "https://connectivitycheck.gstatic.com/generate_204".into()
}

const fn default_socks_port() -> u16 {
    10808
}
const fn default_http_port() -> u16 {
    10809
}
const fn default_api_port() -> u16 {
    10085
}
const fn default_mtu() -> u16 {
    1500
}
const fn default_update_hours() -> u16 {
    12
}
const fn default_subscription_update_hours() -> u16 {
    6
}
const fn default_mux_concurrency() -> u16 {
    8
}
const fn default_ping_timeout() -> u8 {
    5
}
const fn default_ping_parallelism() -> u8 {
    8
}
const fn default_traffic_refresh() -> u8 {
    1
}
const fn default_reconnect_delay() -> u8 {
    5
}
fn default_tun_name() -> String {
    "nory0".into()
}
const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    #[default]
    #[serde(alias = "proxy")]
    Tun,
    MihomoTun,
}

impl fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tun => "TUN · Xray (sing-box)",
            Self::MihomoTun => "TUN · Mihomo",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainStrategy {
    AsIs,
    #[default]
    IpIfNonMatch,
    IpOnDemand,
}

impl DomainStrategy {
    pub fn as_xray(self) -> &'static str {
        match self {
            Self::AsIs => "AsIs",
            Self::IpIfNonMatch => "IPIfNonMatch",
            Self::IpOnDemand => "IPOnDemand",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    None,
    Error,
    #[default]
    Warning,
    Info,
}

impl LogLevel {
    pub fn as_xray(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingSettings {
    /// Built-in category-ru domains and RU IP ranges. Opt-in, persisted locally.
    #[serde(default)]
    pub bypass_ru: bool,
    #[serde(default)]
    pub applications: Vec<ApplicationRule>,
    #[serde(default)]
    pub bypass_domains: Vec<String>,
    #[serde(default)]
    pub bypass_geodata: Vec<GeoDataRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoDataRule {
    pub id: String,
    pub display_name: String,
    pub file_name: String,
    pub kind: GeoDataKind,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoDataKind {
    #[default]
    GeoSite,
    GeoIp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationRule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub source: ApplicationSource,
    #[serde(default)]
    pub bypass: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationSource {
    Steam,
    #[default]
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub source_format: ProfileFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_config: Option<serde_json::Value>,
    pub address: String,
    pub port: u16,
    pub connection: ConnectionSpec,
    #[serde(default)]
    pub stream: StreamSettings,
    pub subscription_id: Option<Uuid>,
    #[serde(default)]
    pub favorite: bool,
    pub latency_ms: Option<u32>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileFormat {
    #[default]
    Link,
    Json,
}

impl Profile {
    pub fn protocol(&self) -> &str {
        match &self.connection {
            ConnectionSpec::Vless { .. } => "VLESS",
            ConnectionSpec::Vmess { .. } => "VMess",
            ConnectionSpec::Trojan { .. } => "Trojan",
            ConnectionSpec::Shadowsocks { .. } => "Shadowsocks",
            ConnectionSpec::Socks { .. } => "SOCKS",
            ConnectionSpec::Hysteria2 { .. } => "Hysteria2",
            ConnectionSpec::XrayJson { name } => name,
        }
    }

    pub fn endpoint_key(&self) -> String {
        // A shared CDN address/port does not identify a profile. Credentials,
        // transport and complete JSON routing may all differ at that endpoint.
        use sha2::{Digest, Sha256};
        let identity = serde_json::to_vec(&(
            self.address.to_lowercase(),
            self.port,
            &self.connection,
            &self.stream,
            &self.raw_config,
        ))
        .expect("profile identity is serializable");
        format!("{:x}", Sha256::digest(identity))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum ConnectionSpec {
    Vless {
        id: String,
        encryption: String,
        flow: Option<String>,
    },
    Vmess {
        id: String,
        alter_id: u32,
        security: String,
    },
    Trojan {
        password: String,
    },
    Shadowsocks {
        method: String,
        password: String,
    },
    Socks {
        username: Option<String>,
        password: Option<String>,
    },
    Hysteria2 {
        auth: String,
    },
    /// An outbound that is executed from its preserved source Xray JSON.
    XrayJson {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSettings {
    #[serde(default)]
    pub network: Transport,
    #[serde(default)]
    pub security: TransportSecurity,
    pub server_name: Option<String>,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub alpn: Vec<String>,
    pub public_key: Option<String>,
    pub short_id: Option<String>,
    pub spider_x: Option<String>,
    pub path: Option<String>,
    pub host: Option<String>,
    pub service_name: Option<String>,
    pub mode: Option<String>,
    pub header_type: Option<String>,
    pub packet_encoding: Option<String>,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            network: Transport::Raw,
            security: TransportSecurity::None,
            server_name: None,
            fingerprint: None,
            alpn: Vec::new(),
            public_key: None,
            short_id: None,
            spider_x: None,
            path: None,
            host: None,
            service_name: None,
            mode: None,
            header_type: None,
            packet_encoding: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Raw,
    Websocket,
    Grpc,
    Xhttp,
    Httpupgrade,
    Mkcp,
    Hysteria,
}

impl Transport {
    pub fn from_share(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "ws" | "websocket" => Self::Websocket,
            "grpc" => Self::Grpc,
            "xhttp" | "splithttp" => Self::Xhttp,
            "httpupgrade" | "http_upgrade" => Self::Httpupgrade,
            "kcp" | "mkcp" => Self::Mkcp,
            "hysteria" | "hysteria2" | "hy2" => Self::Hysteria,
            _ => Self::Raw,
        }
    }

    pub fn as_xray(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Websocket => "websocket",
            Self::Grpc => "grpc",
            Self::Xhttp => "xhttp",
            Self::Httpupgrade => "httpupgrade",
            Self::Mkcp => "mkcp",
            Self::Hysteria => "hysteria",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportSecurity {
    #[default]
    None,
    Tls,
    Reality,
}

impl TransportSecurity {
    pub fn from_share(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "tls" => Self::Tls,
            "reality" => Self::Reality,
            _ => Self::None,
        }
    }

    pub fn as_xray(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tls => "tls",
            Self::Reality => "reality",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    /// Original provider JSON, only exposed to the UI on explicit request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_json: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub support_url: Option<String>,
    pub updated_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub upload_bytes: Option<u64>,
    pub download_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    #[serde(default = "default_true")]
    pub send_hwid: bool,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub provider_update_interval_hours: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error,
}

#[derive(Debug, Clone)]
pub struct ConnectionStatus {
    pub phase: ConnectionPhase,
    pub profile_id: Option<Uuid>,
    pub profile_name: Option<String>,
    pub mode: Option<ConnectionMode>,
    pub started_at: Option<std::time::Instant>,
    pub error: Option<String>,
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self {
            phase: ConnectionPhase::Disconnected,
            profile_id: None,
            profile_name: None,
            mode: None,
            started_at: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreManifest {
    pub version: String,
    pub directory: String,
    pub installed_at: i64,
    pub sha256: String,
}
