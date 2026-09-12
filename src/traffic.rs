//! Session totals from the cores. Linux auto_redirect can carry TCP without
//! crossing the TUN device, so sysfs interface counters undercount that traffic.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{collections::HashMap, io::Read, sync::OnceLock, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    Xray,
    Mihomo,
}

#[derive(Default)]
pub(crate) struct Cache {
    pub checked_at: Option<std::time::Instant>,
    pub totals: (u64, u64),
}

#[derive(Deserialize, Default)]
struct Directions {
    #[serde(default)]
    uplink: u64,
    #[serde(default)]
    downlink: u64,
}
#[derive(Deserialize)]
struct Stats {
    inbound: HashMap<String, Directions>,
}
#[derive(Deserialize)]
struct XrayMetrics {
    stats: Stats,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MihomoTotals {
    upload_total: u64,
    download_total: u64,
}

fn parse(backend: Backend, bytes: &[u8]) -> Result<(u64, u64)> {
    Ok(match backend {
        Backend::Xray => {
            let metrics: XrayMetrics =
                serde_json::from_slice(bytes).context("некорректная статистика Xray")?;
            // Count the client inbound once, never its outbounds or API traffic.
            let counts = metrics.stats.inbound.get("proxy-in");
            counts.map_or((0, 0), |c| (c.uplink, c.downlink))
        }
        Backend::Mihomo => {
            let totals: MihomoTotals =
                serde_json::from_slice(bytes).context("некорректная статистика Mihomo")?;
            (totals.upload_total, totals.download_total)
        }
    })
}

pub(crate) fn read(backend: Backend, port: u16) -> Result<(u64, u64)> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    let client = match CLIENT.get() {
        Some(client) => client,
        None => {
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_millis(800))
                .connect_timeout(Duration::from_millis(300))
                .build()?;
            let _ = CLIENT.set(client);
            CLIENT
                .get()
                .context("не удалось создать клиент статистики")?
        }
    };
    let path = match backend {
        Backend::Xray => "debug/vars",
        Backend::Mihomo => "connections",
    };
    let response = client
        .get(format!("http://127.0.0.1:{port}/{path}"))
        .send()?
        .error_for_status()?;
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    response.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("ответ статистики превышает 8 МБ");
    }
    parse(backend, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xray_counts_client_inbound_once_and_preserves_direction() {
        let data = br#"{"stats":{"inbound":{"proxy-in":{"uplink":17,"downlink":1048576},"api":{"uplink":9999,"downlink":9999}},"outbound":{"proxy":{"uplink":17,"downlink":1048576}}}}"#;
        assert_eq!(parse(Backend::Xray, data).unwrap(), (17, 1048576));
        assert_eq!(
            parse(Backend::Xray, br#"{"stats":{"inbound":{}}}"#).unwrap(),
            (0, 0)
        );
        assert!(parse(Backend::Xray, br#"{"error":"unavailable"}"#).is_err());
    }
    #[test]
    fn mihomo_uses_totals_including_closed_connections_not_rates() {
        let data =
            br#"{"uploadTotal":2048,"downloadTotal":8388608,"up":0,"down":0,"connections":[]}"#;
        assert_eq!(parse(Backend::Mihomo, data).unwrap(), (2048, 8388608));
        assert!(parse(Backend::Mihomo, br#"{"up":23,"down":456}"#).is_err());
    }
    #[test]
    #[ignore = "queries only the isolated test core selected by NORY_TRAFFIC_TEST_PORT"]
    fn reads_isolated_core_totals() {
        let backend = match std::env::var("NORY_TRAFFIC_TEST_CORE").unwrap().as_str() {
            "xray" => Backend::Xray,
            "mihomo" => Backend::Mihomo,
            _ => panic!("test core"),
        };
        let port = std::env::var("NORY_TRAFFIC_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let totals = read(backend, port).unwrap();
        println!("NORY_TRAFFIC_TOTALS={},{}", totals.0, totals.1);
    }
}
