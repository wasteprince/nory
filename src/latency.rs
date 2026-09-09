//! Endpoint and end-to-end probes, always separate from the active TUN.
use crate::{
    models::{ConnectionMode, PingType, Profile, Settings},
    network::DirectRoute,
    storage::Paths,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn validate_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value).context("Некорректный адрес прокси-пинга")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Адрес прокси-пинга должен быть HTTPS-ссылкой без логина и пароля");
    }
    Ok(())
}

pub fn measure(
    profile: &Profile,
    settings: &Settings,
    paths: &Paths,
    route: &DirectRoute,
) -> Result<u32> {
    let timeout = Duration::from_secs(u64::from(settings.ping_timeout_seconds.clamp(1, 10)));
    match settings.ping_type {
        PingType::Icmp => icmp(profile, timeout, route),
        PingType::Tcp => {
            let address =
                SocketAddr::new(IpAddr::V4(route.resolve(&profile.address)?), profile.port);
            average(|| {
                route.tcp(address, timeout)?;
                Ok(())
            })
        }
        PingType::Proxy => proxy(profile, settings, paths, route, timeout),
    }
}

fn average(mut request: impl FnMut() -> Result<()>) -> Result<u32> {
    let mut samples = Vec::new();
    for index in 0..5 {
        let started = Instant::now();
        if request().is_ok() {
            samples.push(started.elapsed().as_micros());
        }
        if index < 4 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if samples.is_empty() {
        bail!("Нет ответов на 5 запросов");
    }
    // Sub-millisecond measurements may display 0, never invent a 1 ms result.
    Ok((samples.iter().sum::<u128>() / samples.len() as u128 / 1000).min(u32::MAX as u128) as u32)
}

pub(crate) fn icmp(profile: &Profile, timeout: Duration, route: &DirectRoute) -> Result<u32> {
    let address = route.resolve(&profile.address)?;
    #[cfg(target_os = "windows")]
    {
        crate::windows::test_latency_direct(address, timeout, route.source)
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("/usr/bin/ping");
        command
            .args([
                "-4",
                "-n",
                "-q",
                "-c",
                "5",
                "-I",
                &route.interface,
                "-W",
                &timeout.as_secs().max(1).to_string(),
                "-w",
                &(timeout.as_secs() * 5 + 3).to_string(),
                "-i",
                "0.2",
                "--",
                &address.to_string(),
            ])
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        let output = command
            .output()
            .context("Не удалось запустить прямой ICMP")?;
        if !output.status.success() {
            bail!("Нет ICMP-ответа по внешнему интерфейсу");
        }
        crate::core::parse_ping_average(&String::from_utf8_lossy(&output.stdout))
    }
}

struct Probe {
    child: Child,
    #[cfg(target_os = "windows")]
    _job: crate::windows::ProcessJob,
    _directory: tempfile::TempDir,
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn xray_config(profile: &Profile, settings: &Settings, route: &DirectRoute) -> Result<Value> {
    let mut config = crate::config::build_xray_config(profile, settings)?;
    let target = config["outbounds"]
        .as_array()
        .and_then(|outbounds| {
            outbounds.iter().find(|o| {
                !matches!(
                    o["protocol"].as_str(),
                    Some("freedom" | "blackhole" | "dns")
                )
            })
        })
        .and_then(|o| o["tag"].as_str())
        .context("Нет прокси для проверки")?
        .to_owned();
    let balancer = config["routing"]["rules"]
        .as_array()
        .and_then(|rules| {
            rules
                .iter()
                .find_map(|r| r.get("balancerTag").and_then(Value::as_str))
        })
        .map(str::to_string);
    let mut rule = json!({"type":"field", "inboundTag":["proxy-in"], "outboundTag":target});
    if let Some(tag) = balancer {
        rule.as_object_mut().unwrap().remove("outboundTag");
        rule["balancerTag"] = json!(tag);
    }
    // No user/domain/direct routing can turn a blocked proxy into a false success.
    config["routing"]["rules"] = json!([rule]);
    let remote_tags: Vec<String> = config["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| {
            !matches!(
                o["protocol"].as_str(),
                Some("freedom" | "blackhole" | "dns")
            )
        })
        .filter_map(|o| o["tag"].as_str().map(str::to_owned))
        .collect();
    if let Some(groups) = config["routing"]["balancers"].as_array_mut() {
        for group in groups {
            group.as_object_mut().map(|g| g.remove("fallbackTag"));
            let selectors = group["selector"]
                .as_array()
                .context("Балансировщик без selector")?;
            let targets: Vec<_> = remote_tags
                .iter()
                .filter(|tag| {
                    selectors
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|prefix| tag.starts_with(prefix))
                })
                .collect();
            if targets.is_empty() {
                bail!("У балансировщика нет прокси для проверки");
            }
            group["selector"] = json!(targets);
        }
    }
    for outbound in config["outbounds"].as_array_mut().unwrap() {
        if matches!(outbound["protocol"].as_str(), Some("blackhole" | "dns")) {
            continue;
        }
        outbound["sendThrough"] = json!(route.source.to_string());
        outbound["streamSettings"]["sockopt"]["interface"] = json!(route.interface);
        if let Some(options) = outbound["streamSettings"]["sockopt"].as_object_mut() {
            options.remove("mark");
        }
    }
    Ok(config)
}

fn proxy(
    profile: &Profile,
    settings: &Settings,
    paths: &Paths,
    route: &DirectRoute,
    timeout: Duration,
) -> Result<u32> {
    validate_url(&settings.ping_url)?;
    // Reserved loopback ports and a private directory per probe; never use or
    // stop the live VPN's listener, helper, configuration, routes or interface.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let api = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let mut probe_settings = settings.clone();
    probe_settings.socks_port = port;
    probe_settings.api_port = api.local_addr()?.port();
    probe_settings.routing = Default::default();
    let directory = tempfile::Builder::new()
        .prefix("ping-")
        .tempdir_in(&paths.runtime_dir)?;
    let path = directory.path().join("probe.json");
    let log = std::fs::File::create(directory.path().join("core.log"))?;
    let mut command;
    if settings.mode == ConnectionMode::MihomoTun {
        let installation =
            crate::mihomo::installed_mihomo(paths)?.context("Mihomo не установлен")?;
        let mut config = crate::mihomo::build_mihomo_config(profile, &probe_settings, &[])?;
        let target = config["rules"]
            .as_array()
            .and_then(|r| {
                r.iter()
                    .rev()
                    .find_map(|v| v.as_str().and_then(|v| v.strip_prefix("MATCH,")))
            })
            .context("Нет прокси-группы для проверки")?
            .to_string();
        if matches!(target.as_str(), "DIRECT" | "REJECT") {
            bail!("Для пинга нужна прокси-группа");
        }
        config["tun"] = json!({"enable":false});
        config["socks-port"] = json!(port);
        config["bind-address"] = json!("127.0.0.1");
        config["interface-name"] = json!(route.interface);
        let mut direct_names = vec![
            "DIRECT".to_string(),
            "REJECT".into(),
            "REJECT-DROP".into(),
            "PASS".into(),
        ];
        if let Some(proxies) = config["proxies"].as_array_mut() {
            for proxy in proxies {
                if matches!(proxy["type"].as_str(), Some("direct" | "reject")) {
                    if let Some(name) = proxy["name"].as_str() {
                        direct_names.push(name.into());
                    }
                } else {
                    proxy["interface-name"] = json!(route.interface);
                    proxy.as_object_mut().map(|p| p.remove("routing-mark"));
                }
            }
        }
        if direct_names.contains(&target) {
            bail!("Цель пинга не является прокси");
        }
        if let Some(groups) = config["proxy-groups"].as_array_mut() {
            for group in groups {
                if let Some(proxies) = group["proxies"].as_array_mut() {
                    proxies.retain(|p| {
                        !p.as_str()
                            .is_some_and(|p| direct_names.iter().any(|d| d == p))
                    });
                    if proxies.is_empty() && group.get("use").is_none() {
                        bail!("В группе прокси-пинга остались только прямые соединения");
                    }
                }
            }
        }
        config["rules"] = json!([format!("MATCH,{target}")]);
        // Do not inherit UDP/HTTP/redir/transparent listeners from provider JSON.
        let object = config.as_object_mut().unwrap();
        for key in [
            "listeners",
            "port",
            "mixed-port",
            "redir-port",
            "tproxy-port",
            "external-controller",
            "external-controller-tls",
            "external-controller-unix",
            "routing-mark",
        ] {
            object.remove(key);
        }
        config["dns"]
            .as_object_mut()
            .map(|dns| dns.remove("listen"));
        crate::storage::atomic_json(&path, &config)?;
        command = Command::new(installation.binary);
        command.arg("-d").arg(directory.path()).arg("-f").arg(&path);
    } else {
        let installation = crate::updater::installed_core(paths)?.context("Xray не установлен")?;
        let mut config = xray_config(profile, &probe_settings, route)?;
        let assets =
            crate::geodata::prepare(&mut config, profile, &probe_settings, &installation, paths)?;
        crate::storage::atomic_json(&path, &config)?;
        command = Command::new(installation.binary);
        command
            .args(["run", "-c"])
            .arg(&path)
            .env("XRAY_LOCATION_ASSET", assets);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id();
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    return Err(std::io::Error::other("NORY exited"));
                }
                Ok(())
            });
        }
    }
    crate::process::hide_window(&mut command);
    drop(listener);
    drop(api);
    let child = command.spawn()?;
    #[cfg(target_os = "windows")]
    let mut child = child;
    #[cfg(target_os = "windows")]
    let job = match crate::windows::ProcessJob::new().and_then(|job| {
        job.attach(&child)?;
        Ok(job)
    }) {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let mut running = Probe {
        child,
        _directory: directory,
        #[cfg(target_os = "windows")]
        _job: job,
    };
    let address: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if running.child.try_wait()?.is_some() {
            bail!("Ядро прокси-пинга завершилось до запуска");
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            bail!("Ядро прокси-пинга не открыло локальный порт");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    average(|| {
        if running.child.try_wait()?.is_some() {
            bail!("Ядро прокси-пинга остановилось");
        }
        // Fresh connection includes SOCKS, actual proxy handshake and TLS.
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(&proxy_url)?)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .build()?;
        let response = client
            .get(&settings.ping_url)
            .header("Cache-Control", "no-cache")
            .header("User-Agent", format!("NORY/{}", env!("CARGO_PKG_VERSION")))
            .send()?;
        if response.status().as_u16() != 204 {
            bail!("Прокси-проверка не получила HTTP 204");
        }
        Ok(())
    })
}
