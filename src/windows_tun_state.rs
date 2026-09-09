//! Adapter presence is not a running Wintun session. This policy contains no
//! OS calls, so its Windows cases can also be regression-tested on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdapterAvailability {
    Available,
    Busy,
    Foreign,
    Unknown,
}

#[derive(Clone, Copy)]
pub(crate) struct AdapterState {
    // None means that Windows could not map the row to a PnP device. It is
    // not evidence of another driver's ownership.
    pub wintun_driver: Option<bool>,
    pub operational: bool,
    pub media_connected: bool,
}

pub(crate) fn availability(adapter: Option<AdapterState>) -> AdapterAvailability {
    match adapter {
        None => AdapterAvailability::Available,
        Some(state) if state.wintun_driver.is_none() => AdapterAvailability::Unknown,
        Some(state) if state.wintun_driver == Some(false) => AdapterAvailability::Foreign,
        Some(state) if state.operational || state.media_connected => AdapterAvailability::Busy,
        Some(_) => AdapterAvailability::Available,
    }
}

/// Never rename/delete a conflicting adapter. The broker serializes selection
/// with stopping/starting its core, and checks the selected name again at start.
pub(crate) fn select_interface(
    preferred: &str,
    mut inspect: impl FnMut(&str) -> anyhow::Result<AdapterAvailability>,
) -> anyhow::Result<String> {
    match inspect(preferred)? {
        AdapterAvailability::Available => return Ok(preferred.into()),
        AdapterAvailability::Busy => anyhow::bail!(
            "TUN-интерфейс {preferred} активен в другом сеансе. Сначала отключите использующий его VPN"
        ),
        AdapterAvailability::Foreign | AdapterAvailability::Unknown => {}
    }
    for index in 1..=32 {
        let candidate = format!("nory-tun{index}");
        if candidate != preferred && inspect(&candidate)? == AdapterAvailability::Available {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Не удалось выбрать свободное имя TUN. Закройте другие VPN и попробуйте ещё раз")
}

pub(crate) fn netcfg_matches(value: &str, guid: &str) -> bool {
    value
        .trim()
        .trim_matches(['{', '}'])
        .eq_ignore_ascii_case(guid.trim().trim_matches(['{', '}']))
}

pub(crate) fn luid_matches(luid: u64, index: u32, if_type: u32) -> bool {
    index != 0
        && if_type != 0
        && index <= 0x00ff_ffff
        && if_type <= 0xffff
        && luid == (u64::from(if_type) << 48) | (u64::from(index) << 24)
}
