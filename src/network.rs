//! Read-only selection of the physical uplink. Never add routes or modify TUN.
use anyhow::{Context, Result, bail};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct DirectRoute {
    pub interface: String,
    pub source: Ipv4Addr,
    #[cfg(target_os = "windows")]
    pub index: u32,
}

impl DirectRoute {
    #[cfg(target_os = "linux")]
    pub fn discover() -> Result<Self> {
        use serde_json::Value;
        use std::process::Command;
        let output = Command::new("/usr/bin/ip")
            .args(["-j", "-4", "route", "show", "table", "main", "default"])
            .output()?;
        if !output.status.success() {
            bail!("Не удалось прочитать внешний маршрут");
        }
        let mut routes: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        routes.sort_by_key(|r| r["metric"].as_u64().unwrap_or(0));
        for row in routes {
            let Some(interface) = row["dev"].as_str() else {
                continue;
            };
            // sysfs device excludes virtual TUN/loopback, independently of name.
            if !std::path::Path::new("/sys/class/net")
                .join(interface)
                .join("device")
                .exists()
            {
                continue;
            }
            let output = Command::new("/usr/bin/ip")
                .args([
                    "-j", "-4", "addr", "show", "dev", interface, "scope", "global",
                ])
                .output()?;
            let addresses: Value = serde_json::from_slice(&output.stdout)?;
            let source = row["prefsrc"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| {
                    addresses[0]["addr_info"]
                        .as_array()?
                        .iter()
                        .find_map(|a| a["local"].as_str()?.parse().ok())
                });
            if let Some(source) = source {
                return Ok(Self {
                    interface: interface.into(),
                    source,
                });
            }
        }
        bail!("Не найден внешний IPv4-интерфейс: проверка через TUN не выполняется")
    }

    #[cfg(target_os = "windows")]
    pub fn discover() -> Result<Self> {
        crate::windows::direct_route()
    }

    pub fn http(&self, timeout: Duration) -> reqwest::blocking::ClientBuilder {
        let builder = reqwest::blocking::Client::builder()
            .no_proxy()
            .local_address(IpAddr::V4(self.source))
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)));
        #[cfg(target_os = "linux")]
        let builder = builder.interface(&self.interface);
        builder
    }

    pub fn tcp(&self, address: SocketAddr, timeout: Duration) -> Result<TcpStream> {
        if !address.is_ipv4() {
            bail!("Для внешнего IPv4-маршрута нужен IPv4-адрес");
        }
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        self.bind_socket(&socket)?;
        socket.connect_timeout(&address.into(), timeout)?;
        Ok(socket.into())
    }

    fn bind_socket(&self, socket: &socket2::Socket) -> Result<()> {
        #[cfg(target_os = "linux")]
        socket.bind_device(Some(self.interface.as_bytes()))?;
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::{IP_UNICAST_IF, IPPROTO_IP, setsockopt};
            let index = self.index.to_be();
            if unsafe {
                setsockopt(
                    socket.as_raw_socket() as _,
                    IPPROTO_IP,
                    IP_UNICAST_IF,
                    (&index as *const u32).cast(),
                    4,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        socket.bind(&SocketAddr::new(IpAddr::V4(self.source), 0).into())?;
        Ok(())
    }

    // Resolve independently of a TUN fake-IP resolver. Literal addresses require
    // no DNS; HTTPS bootstrap addresses avoid recursively resolving DoH itself.
    pub fn resolve(&self, host: &str) -> Result<Ipv4Addr> {
        if let Ok(ip) = host.parse() {
            return Ok(ip);
        }
        // Direct UDP DNS avoids dependence on a single DoH provider, and cannot
        // be answered by the active TUN's fake-IP resolver.
        for server in [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)] {
            if let Ok(ip) = self.resolve_udp(host, server) {
                return Ok(ip);
            }
        }
        let client = self
            .http(Duration::from_secs(5))
            .resolve("dns.google", "8.8.8.8:443".parse().unwrap())
            .build()?;
        let value: serde_json::Value = client
            .get("https://dns.google/resolve")
            .query(&[("name", host), ("type", "A")])
            .send()?
            .error_for_status()?
            .json()?;
        value["Answer"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == 1)
            .find_map(|v| v["data"].as_str()?.parse().ok())
            .context("Прямой DNS не вернул IPv4 сервера")
    }

    fn resolve_udp(&self, host: &str, server: Ipv4Addr) -> Result<Ipv4Addr> {
        let id = u16::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..2].try_into().unwrap());
        let mut query = vec![(id >> 8) as u8, id as u8, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        let host = url::Host::parse(host)?.to_string();
        for label in host.trim_end_matches('.').split('.') {
            if label.is_empty() || label.len() > 63 {
                bail!("Некорректное DNS-имя");
            }
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.extend_from_slice(&[0, 0, 1, 0, 1]);
        if query.len() > 512 {
            bail!("Слишком длинное DNS-имя");
        }
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        self.bind_socket(&socket)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        let socket: std::net::UdpSocket = socket.into();
        socket.connect(SocketAddr::new(server.into(), 53))?;
        socket.send(&query)?;
        let mut reply = [0u8; 4096];
        let length = socket.recv(&mut reply)?;
        parse_dns(&reply[..length], id)
    }
}

fn parse_dns(reply: &[u8], id: u16) -> Result<Ipv4Addr> {
    if reply.len() < 12
        || reply[..2] != id.to_be_bytes()
        || reply[2] & 0x82 != 0x80
        || reply[3] & 15 != 0
    {
        bail!("Некорректный ответ DNS");
    }
    fn name(bytes: &[u8], cursor: &mut usize) -> Result<()> {
        loop {
            let count = *bytes.get(*cursor).context("Оборванное DNS-имя")? as usize;
            *cursor += 1;
            if count == 0 {
                return Ok(());
            }
            if count & 0xc0 == 0xc0 {
                bytes.get(*cursor).context("Оборванный DNS-указатель")?;
                *cursor += 1;
                return Ok(());
            }
            if count > 63 {
                bail!("Некорректная DNS-метка");
            }
            *cursor += count;
            if *cursor > bytes.len() {
                bail!("Оборванное DNS-имя");
            }
        }
    }
    let mut cursor = 12;
    let questions = u16::from_be_bytes([reply[4], reply[5]]);
    if questions != 1 {
        bail!("Неверное число DNS-вопросов");
    }
    name(reply, &mut cursor)?;
    cursor += 4;
    for _ in 0..u16::from_be_bytes([reply[6], reply[7]]) {
        name(reply, &mut cursor)?;
        let record = reply
            .get(cursor..cursor + 10)
            .context("Оборванная DNS-запись")?;
        let length = u16::from_be_bytes([record[8], record[9]]) as usize;
        cursor += 10;
        let data = reply
            .get(cursor..cursor + length)
            .context("Оборванные DNS-данные")?;
        if record[..4] == [0, 1, 0, 1] && length == 4 {
            return Ok(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        cursor += length;
    }
    bail!("DNS не вернул IPv4-адрес")
}
