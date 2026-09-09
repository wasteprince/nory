use crate::config::build_xray_config;
use crate::mihomo::build_mihomo_config;
use crate::models::{ConnectionMode, ConnectionPhase, ConnectionStatus, Profile, Settings};
use crate::privileged;
use crate::process::hide_window;
use crate::storage::{Paths, atomic_json};
use crate::updater::CoreInstallation;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAX_LOG_LINES: usize = 1_000;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: i64,
    pub level: String,
    pub message: String,
}

struct RunningCore {
    child: Child,
    #[cfg(target_os = "windows")]
    _job: crate::windows::ProcessJob,
}

impl Drop for RunningCore {
    fn drop(&mut self) {
        // Child does not terminate its process on drop. Cover every early
        // return during startup, including failures of try_wait/readiness.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

enum RunningBackend {
    Xray {
        process: Option<RunningCore>,
        tun_interface: String,
    },
    Mihomo {
        tun_interface: String,
    },
}

struct CoreState {
    backend: Option<RunningBackend>,
    status: ConnectionStatus,
    api_port: u16,
    generation: u64,
}

pub struct CoreManager {
    paths: Paths,
    lifecycle: Mutex<()>,
    state: Mutex<CoreState>,
    logs: Arc<Mutex<VecDeque<LogEntry>>>,
    log_revision: Arc<AtomicU64>,
}

impl CoreManager {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            lifecycle: Mutex::new(()),
            state: Mutex::new(CoreState {
                backend: None,
                status: ConnectionStatus::default(),
                api_port: 10085,
                generation: 0,
            }),
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES))),
            log_revision: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn connect_xray_tun(
        &self,
        installation: &CoreInstallation,
        profile: &Profile,
        settings: &Settings,
        bypass_processes: &[String],
    ) -> Result<ConnectionStatus> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка управления ядрами"))?;
        self.disconnect_inner()?;
        let generation = self.begin_connection(profile, ConnectionMode::Tun)?;

        let tun_interface = match privileged::configure_tun(
            &settings.tun_interface_name,
            settings.enable_ipv6,
            settings.tun_auto_route,
            settings.mtu,
            settings.socks_port,
            bypass_processes,
        ) {
            Ok(name) => name,
            Err(error) => {
                let error = error.context("Не удалось запустить TUN sing-box для Xray");
                self.finish_connection_error(generation, &error);
                return Err(error);
            }
        };
        self.log_tun_interface(settings, &tun_interface);
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
            state.backend = Some(RunningBackend::Xray {
                process: None,
                tun_interface: tun_interface.clone(),
            });
        }

        match self.start(installation, profile, settings) {
            Ok(running) => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
                if state.generation != generation {
                    drop(state);
                    let mut running = running;
                    let _ = stop_process(&mut running);
                    bail!("запуск VPN был отменён");
                }
                state.api_port = settings.api_port;
                state.backend = Some(RunningBackend::Xray {
                    process: Some(running),
                    tun_interface,
                });
                state.status.phase = ConnectionPhase::Connected;
                state.status.started_at = Some(Instant::now());
                let status = state.status.clone();
                drop(state);
                self.push_log(
                    "info",
                    format!(
                        "Подключён профиль «{}» ({})",
                        profile.name,
                        ConnectionMode::Tun
                    ),
                );
                Ok(status)
            }
            Err(error) => {
                // The sing-box TUN intentionally remains active. With strict
                // routing this blocks traffic instead of leaking it directly.
                self.finish_connection_error(generation, &error);
                Err(error)
            }
        }
    }

    pub fn connect_mihomo_tun(
        &self,
        profile: &Profile,
        settings: &Settings,
        bypass_processes: &[String],
    ) -> Result<ConnectionStatus> {
        let config = build_mihomo_config(profile, settings, bypass_processes)?;
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка управления ядрами"))?;
        self.disconnect_inner()?;
        let generation = self.begin_connection(profile, ConnectionMode::MihomoTun)?;
        let tun_interface = match privileged::configure_mihomo(&config) {
            Ok(name) => name,
            Err(error) => {
                self.finish_connection_error(generation, &error);
                return Err(error);
            }
        };
        self.log_tun_interface(settings, &tun_interface);

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
        if state.generation != generation {
            drop(state);
            let _ = privileged::cleanup_tun();
            bail!("запуск VPN был отменён");
        }
        state.backend = Some(RunningBackend::Mihomo { tun_interface });
        state.status.phase = ConnectionPhase::Connected;
        state.status.started_at = Some(Instant::now());
        let status = state.status.clone();
        drop(state);
        self.push_log(
            "info",
            format!(
                "Подключён профиль «{}» ({})",
                profile.name,
                ConnectionMode::MihomoTun
            ),
        );
        Ok(status)
    }

    fn log_tun_interface(&self, settings: &Settings, actual: &str) {
        if actual != settings.tun_interface_name {
            self.push_log("info", format!(
                "Имя {} занято или его драйвер не определён. Для VPN выбран {actual}; существующий адаптер не изменён",
                settings.tun_interface_name,
            ));
        }
    }

    fn begin_connection(&self, profile: &Profile, mode: ConnectionMode) -> Result<u64> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
        state.generation = state.generation.wrapping_add(1);
        state.status = ConnectionStatus {
            phase: ConnectionPhase::Connecting,
            profile_id: Some(profile.id),
            profile_name: Some(profile.name.clone()),
            mode: Some(mode),
            started_at: None,
            error: None,
        };
        Ok(state.generation)
    }

    fn finish_connection_error(&self, generation: u64, error: &anyhow::Error) {
        let detail = redact(&format!("{error:#}"));
        if let Ok(mut state) = self.state.lock()
            && state.generation == generation
        {
            state.status.phase = ConnectionPhase::Error;
            state.status.error = Some(detail.clone());
        }
        self.push_log("error", detail);
    }

    fn start(
        &self,
        installation: &CoreInstallation,
        profile: &Profile,
        settings: &Settings,
    ) -> Result<RunningCore> {
        let _ = self.prepare_geodata_assets(installation, settings)?;
        let mut config = build_xray_config(profile, settings)?;
        let asset_directory = Some(crate::geodata::prepare(
            &mut config,
            profile,
            settings,
            installation,
            &self.paths,
        )?);
        atomic_json(&self.paths.generated_config(), &config)?;
        validate_config(
            installation,
            &self.paths.generated_config(),
            asset_directory.as_deref(),
        )?;

        let socks_address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, settings.socks_port);
        if TcpStream::connect_timeout(&socks_address.into(), Duration::from_millis(150)).is_ok() {
            bail!(
                "локальный SOCKS-порт 127.0.0.1:{} уже занят другой программой",
                settings.socks_port
            );
        }

        let mut command = Command::new(&installation.binary);
        command
            .args(["run", "-c"])
            .arg(self.paths.generated_config())
            .current_dir(&installation.directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = asset_directory {
            command.env("XRAY_LOCATION_ASSET", directory);
        }
        hide_window(&mut command);
        let mut child = command.spawn().context("не удалось запустить Xray")?;
        #[cfg(target_os = "windows")]
        let job = {
            let result = crate::windows::ProcessJob::new().and_then(|job| {
                job.attach(&child)?;
                Ok(job)
            });
            match result {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(
                        error.context("не удалось защитить процесс Xray от зависания после выхода")
                    );
                }
            }
        };

        let mut running = RunningCore {
            child,
            #[cfg(target_os = "windows")]
            _job: job,
        };
        if let Some(stdout) = running.child.stdout.take() {
            capture_lines(
                stdout,
                Arc::clone(&self.logs),
                Arc::clone(&self.log_revision),
                "info",
            );
        }
        if let Some(stderr) = running.child.stderr.take() {
            capture_lines(
                stderr,
                Arc::clone(&self.logs),
                Arc::clone(&self.log_revision),
                "warning",
            );
        }
        for _ in 0..200 {
            if let Some(status) = running.child.try_wait()? {
                std::thread::sleep(Duration::from_millis(50));
                let detail = self
                    .logs
                    .lock()
                    .ok()
                    .and_then(|logs| logs.back().map(|entry| entry.message.clone()))
                    .unwrap_or_else(|| "ядро не записало подробности".into());
                bail!(
                    "Xray завершился при запуске (код {}): {detail}",
                    status
                        .code()
                        .map_or_else(|| "signal".into(), |code| code.to_string())
                );
            }
            if TcpStream::connect_timeout(&socks_address.into(), Duration::from_millis(100)).is_ok()
            {
                return Ok(running);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        stop_process(&mut running)?;
        let detail = self
            .logs
            .lock()
            .ok()
            .and_then(|logs| logs.back().map(|entry| entry.message.clone()))
            .unwrap_or_else(|| "ядро не записало подробности".into());
        bail!(
            "Xray не открыл SOCKS-порт 127.0.0.1:{} за 10 секунд: {detail}",
            settings.socks_port
        )
    }

    fn prepare_geodata_assets(
        &self,
        installation: &CoreInstallation,
        settings: &Settings,
    ) -> Result<Option<std::path::PathBuf>> {
        if settings.routing.bypass_geodata.is_empty() {
            return Ok(None);
        }
        let directory = self.paths.geodata_dir();
        fs::create_dir_all(&directory)
            .with_context(|| format!("не удалось создать {}", directory.display()))?;
        for name in ["geoip.dat", "geosite.dat"] {
            let source = installation.directory.join(name);
            let target = directory.join(name);
            copy_asset_if_changed(&source, &target)?;
        }
        for rule in &settings.routing.bypass_geodata {
            let file_name = std::path::Path::new(&rule.file_name);
            if file_name.file_name() != Some(file_name.as_os_str()) {
                bail!("некорректное имя GeoData: {}", rule.file_name);
            }
            let path = directory.join(file_name);
            let metadata = fs::metadata(&path)
                .with_context(|| format!("файл GeoData не найден: {}", rule.display_name))?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
                bail!("файл GeoData некорректен: {}", rule.display_name);
            }
        }
        Ok(Some(directory))
    }

    pub fn disconnect(&self) -> Result<ConnectionStatus> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка управления ядрами"))?;
        self.disconnect_inner()
    }

    /// Cleanup can still be pending even when reconnecting is disabled.
    pub fn needs_disconnect(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.backend.is_some()
                || (state.status.mode.is_some()
                    && state.status.phase != ConnectionPhase::Disconnected)
        })
    }

    fn disconnect_inner(&self) -> Result<ConnectionStatus> {
        self.disconnect_with_cleanup(privileged::cleanup_tun)
    }

    fn disconnect_with_cleanup(
        &self,
        cleanup: impl FnOnce() -> Result<()>,
    ) -> Result<ConnectionStatus> {
        let (generation, mut backend, should_cleanup_tun, previous_status) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
            state.generation = state.generation.wrapping_add(1);
            let backend = state.backend.take();
            let should_cleanup_tun = backend.is_some()
                || state.status.mode.is_some()
                    && state.status.phase != ConnectionPhase::Disconnected;
            let previous_status = state.status.clone();
            if should_cleanup_tun {
                state.status.phase = ConnectionPhase::Disconnecting;
            }
            (
                state.generation,
                backend,
                should_cleanup_tun,
                previous_status,
            )
        };

        let mut result = Ok(());
        if let Some(RunningBackend::Xray { process, .. }) = backend.as_mut()
            && let Some(running) = process.as_mut()
        {
            result = stop_process(running);
            if result.is_ok() {
                *process = None;
            }
        }
        if should_cleanup_tun {
            let tun_result = cleanup();
            if result.is_ok() {
                result = tun_result;
            }
        }
        if result.is_ok() && should_cleanup_tun {
            self.push_log("info", "Соединение отключено".into());
        }
        if let Err(error) = &result {
            self.push_log(
                "error",
                format!("Не удалось полностью отключить VPN: {error:#}. Повторите отключение"),
            );
        }
        let next_status = disconnect_status(previous_status, &result);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
        if state.generation == generation {
            state.status = next_status;
            // Do not lose ownership on an RPC timeout or process-stop error.
            // A second disconnect (including exit) must retry the same cleanup.
            if result.is_err() {
                state.backend = backend;
            }
        }
        let status = state.status.clone();
        drop(state);
        result?;
        Ok(status)
    }

    pub fn status(&self) -> ConnectionStatus {
        let Ok(mut state) = self.state.lock() else {
            return ConnectionStatus {
                phase: ConnectionPhase::Error,
                error: Some("внутренняя ошибка состояния".into()),
                ..ConnectionStatus::default()
            };
        };
        if let Some(RunningBackend::Xray {
            process: Some(process),
            ..
        }) = state.backend.as_mut()
            && let Ok(Some(exit)) = process.child.try_wait()
        {
            if let Some(RunningBackend::Xray { process, .. }) = state.backend.as_mut() {
                *process = None;
            }
            state.status.phase = ConnectionPhase::Error;
            state.status.error = Some(format!("Xray неожиданно завершился ({exit})"));
        }
        #[cfg(target_os = "linux")]
        if let Some(RunningBackend::Mihomo { tun_interface }) = state.backend.as_ref()
            && !std::path::Path::new("/sys/class/net")
                .join(tun_interface)
                .exists()
        {
            state.backend = None;
            state.status.phase = ConnectionPhase::Error;
            state.status.error = Some("Mihomo неожиданно завершился".into());
        }
        #[cfg(target_os = "windows")]
        if let Some(backend) = state.backend.as_ref() {
            let (RunningBackend::Xray { tun_interface, .. }
            | RunningBackend::Mihomo { tun_interface }) = backend;
            if state.status.phase == ConnectionPhase::Connected
                // A registered adapter can outlive its process, and conversely
                // Wintun can report Down with a live session. Ask its owner;
                // a transient RPC error is not proof the tunnel has stopped.
                && (matches!(crate::windows_service::tunnel_alive(tun_interface), Ok(false))
                    || crate::windows::interface_row(tun_interface).is_err())
            {
                state.status.phase = ConnectionPhase::Error;
                state.status.error = Some(
                    "TUN-интерфейс неожиданно остановлен. Попробуйте подключиться ещё раз".into(),
                );
            }
        }
        let status = state.status.clone();
        drop(state);
        status
    }

    pub fn report_connection_error(
        &self,
        profile: &Profile,
        mode: crate::models::ConnectionMode,
        error: &str,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let already_recorded = state.status.phase == ConnectionPhase::Error
            && state.status.error.as_deref() == Some(error);
        state.status = ConnectionStatus {
            phase: ConnectionPhase::Error,
            profile_id: Some(profile.id),
            profile_name: Some(profile.name.clone()),
            mode: Some(mode),
            started_at: None,
            error: Some(error.to_string()),
        };
        drop(state);
        if !already_recorded {
            self.push_log("error", error.to_string());
        }
    }

    pub fn traffic(&self) -> Result<(u64, u64)> {
        let interface = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("внутренняя ошибка состояния"))?;
            match state.backend.as_ref().context("TUN-ядро не запущено")? {
                RunningBackend::Xray { tun_interface, .. }
                | RunningBackend::Mihomo { tun_interface } => tun_interface.clone(),
            }
        };
        #[cfg(target_os = "linux")]
        {
            let statistics = std::path::Path::new("/sys/class/net")
                .join(&interface)
                .join("statistics");
            let uplink = read_counter(&statistics.join("tx_bytes"))?;
            let downlink = read_counter(&statistics.join("rx_bytes"))?;
            return Ok((uplink, downlink));
        }
        #[cfg(target_os = "windows")]
        {
            crate::windows::traffic(&interface)
        }
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        self.logs
            .lock()
            .map(|logs| logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear_logs(&self) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.clear();
            self.log_revision.fetch_add(1, Ordering::Release);
        }
    }

    pub fn logs_revision(&self) -> u64 {
        self.log_revision.load(Ordering::Acquire)
    }

    pub(crate) fn push_log(&self, level: &str, message: String) {
        push_log(&self.logs, &self.log_revision, level, redact(&message));
    }
}

fn disconnect_status(previous: ConnectionStatus, result: &Result<()>) -> ConnectionStatus {
    match result {
        Ok(()) => ConnectionStatus::default(),
        Err(error) => ConnectionStatus {
            phase: ConnectionPhase::Error,
            error: Some(format!(
                "Не удалось полностью отключить VPN: {error:#}. Повторите отключение"
            )),
            ..previous
        },
    }
}

fn copy_asset_if_changed(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    let source_metadata = fs::metadata(source)
        .with_context(|| format!("встроенный GeoData не найден: {}", source.display()))?;
    let unchanged = fs::metadata(target)
        .is_ok_and(|target_metadata| target_metadata.len() == source_metadata.len());
    if !unchanged {
        let temporary = target.with_extension("dat.tmp");
        fs::copy(source, &temporary)
            .with_context(|| format!("не удалось подготовить {}", source.display()))?;
        fs::rename(&temporary, target)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_counter(path: &std::path::Path) -> Result<u64> {
    std::fs::read_to_string(path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?
        .trim()
        .parse()
        .context("некорректный сетевой счётчик")
}

impl Drop for CoreManager {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

#[cfg(target_os = "linux")]
pub fn test_latency(profile: &Profile, timeout: Duration) -> Result<u32> {
    let route = crate::network::DirectRoute::discover()?;
    crate::latency::icmp(profile, timeout, &route)
}

#[cfg(target_os = "windows")]
pub fn test_latency(profile: &Profile, timeout: Duration) -> Result<u32> {
    crate::windows::test_latency(profile.address.trim(), timeout)
}

pub(crate) fn parse_ping_average(output: &str) -> Result<u32> {
    let average = output
        .lines()
        .find(|line| line.contains("min/avg/max"))
        .and_then(|line| line.split_once('=').map(|(_, values)| values.trim()))
        .and_then(|values| values.split('/').nth(1))
        .and_then(|average| average.trim().parse::<f64>().ok())
        .filter(|average| average.is_finite() && *average >= 0.0)
        .context("ping не вернул среднюю задержку")?;
    Ok(average.round().clamp(0.0, u32::MAX as f64) as u32)
}

fn validate_config(
    installation: &CoreInstallation,
    config: &std::path::Path,
    asset_directory: Option<&std::path::Path>,
) -> Result<()> {
    let mut command = Command::new(&installation.binary);
    command
        .args(["run", "-test", "-c"])
        .arg(config)
        .current_dir(&installation.directory)
        .stdin(Stdio::null());
    if let Some(directory) = asset_directory {
        command.env("XRAY_LOCATION_ASSET", directory);
    }
    hide_window(&mut command);
    let output = command_output_with_timeout(command, Duration::from_secs(10))
        .context("не удалось проверить конфигурацию Xray")?;
    if !output.status.success() {
        let raw = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let message = core_error_detail(&raw);
        bail!("Xray отклонил конфигурацию: {}", message);
    }
    Ok(())
}

fn core_error_detail(output: &str) -> String {
    // stdout and stderr both commonly end with a newline. Choosing the final
    // line before trimming used to show an empty error instead of the cause.
    redact(
        output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("ядро не записало подробности"),
    )
}

fn stop_process(running: &mut RunningCore) -> Result<()> {
    if running.child.try_wait()?.is_some() {
        return Ok(());
    }
    running.child.kill()?;
    for _ in 0..20 {
        if running.child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    running
        .child
        .kill()
        .context("Не удалось завершить процесс Xray")?;
    running
        .child
        .wait()
        .context("Не удалось дождаться остановки Xray")?;
    Ok(())
}

fn capture_lines<R: std::io::Read + Send + 'static>(
    reader: R,
    logs: Arc<Mutex<VecDeque<LogEntry>>>,
    revision: Arc<AtomicU64>,
    default_level: &'static str,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let lower = line.to_ascii_lowercase();
            let level = if lower.contains("error") || lower.contains("failed") {
                "error"
            } else if lower.contains("warning") {
                "warning"
            } else {
                default_level
            };
            push_log(&logs, &revision, level, redact(&line));
        }
    });
}

fn push_log(
    logs: &Arc<Mutex<VecDeque<LogEntry>>>,
    revision: &AtomicU64,
    level: &str,
    message: String,
) {
    if let Ok(mut logs) = logs.lock() {
        if logs.len() == MAX_LOG_LINES {
            logs.pop_front();
        }
        logs.push_back(LogEntry {
            at: unix_time(),
            level: level.into(),
            message,
        });
        revision.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    // Drain both pipes immediately. Waiting for the exit first can deadlock
    // Windows' small anonymous-pipe buffers (e.g. GTK loader enumeration).
    fn drain(mut input: impl std::io::Read) -> std::io::Result<Vec<u8>> {
        let mut output = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let size = input.read(&mut chunk)?;
            if size == 0 {
                return Ok(output);
            }
            let keep = size.min((1024usize * 1024).saturating_sub(output.len()));
            output.extend_from_slice(&chunk[..keep]);
        }
    }
    let stdout = child
        .stdout
        .take()
        .context("нет stdout дочернего процесса")?;
    let stderr = child
        .stderr
        .take()
        .context("нет stderr дочернего процесса")?;
    let stdout = std::thread::spawn(move || drain(stdout));
    let stderr = std::thread::spawn(move || drain(stderr));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(std::process::Output {
                status,
                stdout: stdout
                    .join()
                    .map_err(|_| anyhow::anyhow!("сбой чтения stdout"))??,
                stderr: stderr
                    .join()
                    .map_err(|_| anyhow::anyhow!("сбой чтения stderr"))??,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("процесс не завершился за {} с", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn redact(message: &str) -> String {
    let mut result = message.to_string();
    for scheme in ["vless://", "vmess://", "trojan://", "ss://", "socks://"] {
        while let Some(start) = result.find(scheme) {
            let end = result[start..]
                .find(char::is_whitespace)
                .map(|index| start + index)
                .unwrap_or(result.len());
            result.replace_range(start..end, "[скрытая ссылка]");
        }
    }
    result.chars().take(2_000).collect()
}

fn parse_stats(bytes: &[u8]) -> Result<(u64, u64)> {
    let value: Value = serde_json::from_slice(bytes).context("некорректная статистика Xray")?;
    let mut uplink = 0_u64;
    let mut downlink = 0_u64;
    if let Some(stats) = value.get("stat").and_then(Value::as_array) {
        for stat in stats {
            let name = stat.get("name").and_then(Value::as_str).unwrap_or_default();
            let count = stat
                .get("value")
                .and_then(|item| item.as_u64().or_else(|| item.as_str()?.parse().ok()))
                .unwrap_or(0);
            if name.ends_with(">>>uplink") {
                uplink = uplink.saturating_add(count);
            }
            if name.ends_with(">>>downlink") {
                downlink = downlink.saturating_add(count);
            }
        }
    }
    Ok((uplink, downlink))
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn profile_by_id(profiles: &[Profile], id: Uuid) -> Result<&Profile> {
    profiles
        .iter()
        .find(|profile| profile.id == id)
        .context("профиль не найден")
}
