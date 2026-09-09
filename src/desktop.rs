//! UI-independent desktop actions. Secrets remain in Rust, not in the WebView.
use crate::{
    app_updater, applications, core::CoreManager, models::*, privileged, storage, subscription,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

pub struct Desktop {
    pub core: Arc<CoreManager>,
    paths: storage::Paths,
    data: Mutex<AppData>,
    changes: Mutex<()>,
    network_busy: AtomicBool,
    update: Mutex<Option<app_updater::AppRelease>>,
    want_connection: AtomicBool,
    stopping: AtomicBool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Snapshot,
    Poll,
    Logs,
    ClearLogs,
    Connect,
    Disconnect,
    SelectProfile { id: Uuid },
    SelectSubscription { id: Option<Uuid> },
    AddSubscription { url: String, send_hwid: bool },
    RefreshSubscription { id: Uuid },
    SubscriptionUrl { id: Uuid },
    SubscriptionJson { id: Uuid },
    ChangeSubscriptionUrl { id: Uuid, url: String },
    DeleteSubscription { id: Uuid },
    ImportLinks { text: String },
    Ping { subscription_id: Option<Uuid> },
    Catalog { processes: bool },
    SaveSettings { settings: Settings },
    AddBypass { name: String, matcher: String },
    ToggleBypass { id: String, enabled: bool },
    DeleteBypass { id: String },
    CheckUpdate,
}

#[derive(Serialize)]
struct ProfileView {
    id: Uuid,
    name: String,
    description: Option<String>,
    protocol: String,
    transport: String,
    security: String,
    format: ProfileFormat,
    subscription_id: Option<Uuid>,
    latency_ms: Option<u32>,
    favorite: bool,
    flag_image: Option<String>,
}

struct NetworkGuard<'a>(&'a AtomicBool);
impl Drop for NetworkGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Desktop {
    pub fn new(paths: storage::Paths) -> Result<Self> {
        let data = storage::load_state(&paths)?;
        Ok(Self {
            core: Arc::new(CoreManager::new(paths.clone())),
            paths,
            data: Mutex::new(data),
            changes: Mutex::new(()),
            network_busy: AtomicBool::new(false),
            update: Mutex::new(None),
            want_connection: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        })
    }

    pub fn settings(&self) -> Settings {
        self.data
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .settings
            .clone()
    }

    pub fn selected_subscription(&self) -> Option<Uuid> {
        self.data
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected_subscription
    }

    pub fn due_subscriptions(&self) -> Vec<Uuid> {
        let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        if !data.settings.auto_update_subscriptions {
            return vec![];
        }
        let interval = u64::from(data.settings.subscription_update_interval_hours.max(1)) * 3600;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        data.subscriptions
            .iter()
            .filter(|s| {
                s.updated_at
                    .is_none_or(|at| now.saturating_sub(at.max(0) as u64) >= interval)
            })
            .map(|s| s.id)
            .collect()
    }

    pub fn retry_connection(&self) -> Result<()> {
        let Ok(_operation) = self.changes.try_lock() else {
            return Ok(());
        };
        if !self.stopping.load(Ordering::Acquire)
            && self.wants_connection()
            && self.settings().auto_reconnect
            && self.core.status().phase == ConnectionPhase::Error
        {
            self.connect()?;
        }
        Ok(())
    }

    pub fn shutdown(&self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.want_connection.store(false, Ordering::Release);
        let _operation = self
            .changes
            .lock()
            .map_err(|_| anyhow::anyhow!("Ошибка завершения подключения"))?;
        self.core.disconnect().map(|_| ())
    }

    pub fn wants_connection(&self) -> bool {
        self.want_connection.load(Ordering::Acquire)
    }

    pub fn can_disconnect(&self) -> bool {
        self.wants_connection() || self.core.needs_disconnect()
    }

    fn change(&self, f: impl FnOnce(&mut AppData) -> Result<()>) -> Result<()> {
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("Ошибка хранилища"))?;
        let mut next = data.clone();
        f(&mut next)?;
        storage::save_state(&self.paths, &next)?;
        *data = next;
        Ok(())
    }

    pub fn snapshot(&self) -> Value {
        let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        let profiles: Vec<_> = data
            .profiles
            .iter()
            .map(|p| ProfileView {
                id: p.id,
                name: p.name.clone(),
                description: p.description.clone(),
                protocol: p.protocol().into(),
                transport: p.stream.network.as_xray().to_uppercase(),
                security: p.stream.security.as_xray().to_uppercase(),
                format: p.source_format,
                subscription_id: p.subscription_id,
                latency_ms: p.latency_ms,
                favorite: p.favorite,
                flag_image: flag_image(&p.name),
            })
            .collect();
        let subscriptions: Vec<_> = data.subscriptions.iter().map(|s| json!({
            "id":s.id,"name":s.name,"description":s.description,"updated_at":s.updated_at,
            "expires_at":s.expires_at,"upload_bytes":s.upload_bytes,"download_bytes":s.download_bytes,
            "total_bytes":s.total_bytes,"send_hwid":s.send_hwid,
        })).collect();
        json!({"version":env!("CARGO_PKG_VERSION"), "platform":std::env::consts::OS,
            "settings":data.settings,"profiles":profiles,"subscriptions":subscriptions,
            "selected_profile":data.selected_profile,"selected_subscription":data.selected_subscription})
    }

    pub fn poll(&self) -> Value {
        let status = self.core.status();
        let (up, down) = self.core.traffic().unwrap_or_default();
        json!({"phase":match status.phase {
            ConnectionPhase::Disconnected=>"disconnected", ConnectionPhase::Connecting=>"connecting",
            ConnectionPhase::Connected=>"connected", ConnectionPhase::Disconnecting=>"disconnecting", ConnectionPhase::Error=>"error"
        },"error":status.error,"profile_id":status.profile_id,"profile_name":status.profile_name,
            "seconds":status.started_at.map_or(0, |at| at.elapsed().as_secs()),
            "upload":up,"download":down,"logs_revision":self.core.logs_revision(),
            "network_busy":self.network_busy.load(Ordering::Acquire),"vpn_requested":self.can_disconnect()})
    }

    fn connect(&self) -> Result<()> {
        let (profile, settings) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("Ошибка хранилища"))?;
            (
                data.profiles
                    .iter()
                    .find(|p| Some(p.id) == data.selected_profile)
                    .cloned()
                    .context("Выберите сервер")?,
                data.settings.clone(),
            )
        };
        validate_settings(&settings)?;
        let bypass: Vec<_> = settings
            .routing
            .applications
            .iter()
            .filter(|r| r.bypass)
            .flat_map(|r| r.processes.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let result = privileged::prepare_tun_core().and_then(|core| match settings.mode {
            ConnectionMode::Tun => self
                .core
                .connect_xray_tun(&core, &profile, &settings, &bypass),
            ConnectionMode::MihomoTun => self.core.connect_mihomo_tun(&profile, &settings, &bypass),
        });
        // A disconnect queued during startup wins over a successful connection
        // and disables auto-reconnect before waiting for the operation lock.
        if !self.wants_connection() {
            self.core.disconnect()?;
            return Ok(());
        }
        if let Err(error) = &result {
            self.core
                .report_connection_error(&profile, settings.mode, &format!("{error:#}"));
        }
        result.map(|_| ())
    }

    fn reconnect_if_needed(&self, was_connected: bool) -> Result<()> {
        if was_connected {
            self.connect()
                .context("Изменения сохранены, но VPN не удалось перезапустить")?;
        }
        Ok(())
    }

    fn network_guard(&self) -> Result<NetworkGuard<'_>> {
        if self.network_busy.swap(true, Ordering::AcqRel) {
            bail!("Дождитесь завершения обновления или пинга");
        }
        Ok(NetworkGuard(&self.network_busy))
    }

    fn fetch(&self, sub: &Subscription) -> Result<subscription::FetchResult> {
        // Network goes through Rust, never through WebView. Both TUN backends
        // already route the nory/nory.exe process directly.
        let route = crate::network::DirectRoute::discover()?;
        let url = url::Url::parse(&sub.url)?;
        let host = url
            .host_str()
            .context("В подписке отсутствует имя сервера")?;
        let address = route.resolve(host)?;
        let client = route
            .http(Duration::from_secs(45))
            .resolve(
                host,
                std::net::SocketAddr::new(
                    address.into(),
                    url.port_or_known_default().unwrap_or(443),
                ),
            )
            .build()?;
        let hwid = if sub.send_hwid {
            Some(storage::load_or_create_hwid(&self.paths)?)
        } else {
            None
        };
        subscription::fetch(&client, sub, hwid.as_deref())
    }

    pub fn handle(&self, action: Action) -> Result<Value> {
        match action {
            Action::Disconnect => {
                self.want_connection.store(false, Ordering::Release);
                let _operation = self
                    .changes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Ошибка остановки VPN"))?;
                self.core.disconnect()?;
                return Ok(self.snapshot());
            }
            Action::Snapshot => return Ok(self.snapshot()),
            Action::Poll => return Ok(self.poll()),
            Action::Logs => {
                return Ok(json!(
                    self.core
                        .logs()
                        .iter()
                        .map(|l| json!({"at":l.at,"level":l.level,"message":l.message}))
                        .collect::<Vec<_>>()
                ));
            }
            Action::ClearLogs => {
                self.core.clear_logs();
                return Ok(Value::Null);
            }
            Action::Catalog { processes } => {
                let items: Vec<_> = if processes {
                    applications::running_processes()
                        .into_iter()
                        .map(|p| json!({"name":p.name,"matcher":p.matcher}))
                        .collect()
                } else {
                    applications::installed_applications()
                        .into_iter()
                        .map(|p| json!({"name":p.name,"matcher":p.matcher}))
                        .collect()
                };
                return Ok(json!(items));
            }
            Action::CheckUpdate => {
                let release = app_updater::check_for_update()?;
                let result = release
                    .as_ref()
                    .map(|r| json!({"version":r.version,"notes":r.notes}));
                *self.update.lock().unwrap_or_else(|p| p.into_inner()) = release;
                return Ok(json!(result));
            }
            Action::SubscriptionUrl { id } => {
                let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
                let sub = data
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .context("Подписка удалена")?;
                return Ok(json!({"id": id, "name": sub.name, "url": sub.url}));
            }
            Action::SubscriptionJson { id } => {
                let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
                let sub = data
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .context("Подписка удалена")?;
                let original = sub.source_json.is_some();
                let document = sub.source_json.clone().unwrap_or_else(|| {
                    json!(
                        data.profiles
                            .iter()
                            .filter(|p| p.subscription_id == Some(id))
                            .map(|p| p.raw_config.clone().unwrap_or_else(|| json!(p)))
                            .collect::<Vec<_>>()
                    )
                });
                return Ok(json!({"id": id, "name": sub.name, "original": original,
                    "text": serde_json::to_string_pretty(&document)?}));
            }
            Action::ChangeSubscriptionUrl { id, url } => {
                let _network = self.network_guard()?;
                let old = self
                    .data
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .cloned()
                    .context("Подписка удалена")?;
                let mut replacement = subscription::new_subscription(&url, old.send_hwid)?;
                replacement.id = id;
                if replacement.url == old.url {
                    return Ok(self.snapshot());
                }
                if self
                    .data
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .subscriptions
                    .iter()
                    .any(|s| s.id != id && s.url == replacement.url)
                {
                    bail!("Эта ссылка уже используется другой подпиской");
                }
                // No old ETag, no mutation until the new URL has been fetched
                // and parsed successfully. The ID, list position and HWID stay.
                let result = self.fetch(&replacement)?;
                if matches!(result, subscription::FetchResult::NotModified) {
                    bail!("Сервер не вернул содержимое новой подписки");
                }
                self.change(|data| {
                    subscription::apply_url_change(data, &old, replacement, result)
                })?;
                return Ok(self.snapshot());
            }
            Action::AddSubscription { url, send_hwid } => {
                let _network = self.network_guard()?;
                let sub = subscription::new_subscription(&url, send_hwid)?;
                if self
                    .data
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .subscriptions
                    .iter()
                    .any(|s| s.url == sub.url)
                {
                    bail!("Эта подписка уже добавлена");
                }
                let result = self.fetch(&sub)?;
                let keep_selection = self.wants_connection();
                self.change(|data| {
                    let id = sub.id;
                    data.subscriptions.push(sub);
                    subscription::apply_fetch(data, id, result)?;
                    data.selected_subscription = Some(id);
                    if !keep_selection {
                        data.selected_profile = data
                            .profiles
                            .iter()
                            .find(|p| p.subscription_id == Some(id))
                            .map(|p| p.id);
                    }
                    Ok(())
                })?;
                return Ok(self.snapshot());
            }
            Action::RefreshSubscription { id } => {
                let _network = self.network_guard()?;
                let sub = self
                    .data
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .cloned()
                    .context("Подписка удалена")?;
                let result = self.fetch(&sub)?;
                self.change(|data| {
                    subscription::apply_fetch(data, id, result)?;
                    Ok(())
                })?;
                return Ok(self.snapshot());
            }
            Action::Ping { subscription_id } => {
                let _network = self.network_guard()?;
                let (profiles, settings) = {
                    let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
                    (ping_targets(&data, subscription_id)?, data.settings.clone())
                };
                let results = Mutex::new(Vec::new());
                let route = crate::network::DirectRoute::discover()?;
                let next = std::sync::atomic::AtomicUsize::new(0);
                std::thread::scope(|scope| {
                    for _ in 0..usize::from(settings.ping_parallelism.clamp(
                        1,
                        if matches!(settings.ping_type, PingType::Proxy) {
                            2
                        } else {
                            16
                        },
                    ))
                    .min(profiles.len())
                    {
                        let (profiles, next, results, settings, route, paths) =
                            (&profiles, &next, &results, &settings, &route, &self.paths);
                        scope.spawn(move || {
                            loop {
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                let Some(p) = profiles.get(i) else { break };
                                let latency =
                                    match crate::latency::measure(p, settings, paths, route) {
                                        Ok(ms) => Some(ms),
                                        Err(error) => {
                                            self.core.push_log(
                                                "warning",
                                                format!(
                                                    "Пинг {:?} [{}]: {error:#}",
                                                    settings.ping_type, p.id
                                                ),
                                            );
                                            None
                                        }
                                    };
                                results.lock().unwrap().push((p.id, latency));
                            }
                        });
                    }
                });
                let _selection = self
                    .changes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Ошибка выбора сервера"))?;
                let disconnected = self.core.status().phase == ConnectionPhase::Disconnected;
                self.change(|data| {
                    for (id, ms) in results.into_inner().unwrap() {
                        if let Some(p) = data
                            .profiles
                            .iter_mut()
                            .find(|p| p.id == id && p.subscription_id == subscription_id)
                        {
                            p.latency_ms = ms;
                        }
                    }
                    if disconnected
                        && data.settings.auto_select_fastest
                        && data.selected_subscription == subscription_id
                    {
                        if let Some(p) = data
                            .profiles
                            .iter()
                            .filter(|p| {
                                p.subscription_id == subscription_id && p.latency_ms.is_some()
                            })
                            .min_by_key(|p| p.latency_ms)
                        {
                            data.selected_profile = Some(p.id);
                        }
                    }
                    Ok(())
                })?;
                return Ok(self.snapshot());
            }
            action => {
                let _operation = self
                    .changes
                    .try_lock()
                    .map_err(|_| anyhow::anyhow!("Дождитесь завершения подключения"))?;
                if self.stopping.load(Ordering::Acquire) {
                    bail!("NORY завершает работу");
                }
                // Preserve the requested VPN state, including a failed TUN startup.
                let was_connected = self.wants_connection();
                match action {
                    Action::Connect => {
                        self.want_connection.store(true, Ordering::Release);
                        self.connect()?;
                    }
                    Action::SelectProfile { id } => {
                        self.change(|data| {
                            let p = data
                                .profiles
                                .iter()
                                .find(|p| p.id == id)
                                .context("Сервер удалён")?;
                            data.selected_subscription = p.subscription_id;
                            data.selected_profile = Some(id);
                            Ok(())
                        })?;
                        self.reconnect_if_needed(was_connected)?;
                    }
                    Action::SelectSubscription { id } => {
                        self.change(|data| {
                            if id.is_some() && !data.subscriptions.iter().any(|s| Some(s.id) == id)
                            {
                                bail!("Подписка удалена");
                            }
                            data.selected_subscription = id;
                            if !was_connected {
                                data.selected_profile = data
                                    .profiles
                                    .iter()
                                    .find(|p| p.subscription_id == id)
                                    .map(|p| p.id);
                            }
                            Ok(())
                        })?;
                    }
                    Action::DeleteSubscription { id } => {
                        let running = self.core.status().profile_id;
                        let active = self
                            .data
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .profiles
                            .iter()
                            .any(|p| Some(p.id) == running && p.subscription_id == Some(id));
                        if active {
                            self.want_connection.store(false, Ordering::Release);
                            self.core.disconnect()?;
                        }
                        self.change(|data| {
                            data.subscriptions.retain(|s| s.id != id);
                            data.profiles.retain(|p| p.subscription_id != Some(id));
                            repair_selection(data);
                            Ok(())
                        })?;
                    }
                    Action::ImportLinks { text } => {
                        if text.len() > 8 * 1024 * 1024 {
                            bail!("Слишком большой список серверов");
                        }
                        let mut profiles = subscription::parse_profiles(&text)?;
                        self.change(|data| {
                            data.selected_subscription = None;
                            if !was_connected {
                                data.selected_profile = profiles.first().map(|p| p.id);
                            }
                            data.profiles.append(&mut profiles);
                            Ok(())
                        })?;
                    }
                    Action::SaveSettings { settings } => {
                        validate_settings(&settings)?;
                        let old = self.settings().start_at_login;
                        if old != settings.start_at_login {
                            configure_autostart(settings.start_at_login)?;
                        }
                        if let Err(e) = self.change(|data| {
                            data.settings = settings;
                            Ok(())
                        }) {
                            let _ = configure_autostart(old);
                            return Err(e);
                        }
                        self.reconnect_if_needed(was_connected)?;
                    }
                    Action::AddBypass { name, matcher } => {
                        let matcher = matcher.trim().to_string();
                        if matcher.is_empty()
                            || matcher.len() > 4096
                            || matcher.contains(['\0', '\n', '\r'])
                        {
                            bail!("Выберите приложение или процесс");
                        }
                        self.change(|data| {
                            if data
                                .settings
                                .routing
                                .applications
                                .iter()
                                .any(|r| r.processes.contains(&matcher))
                            {
                                bail!("Приложение уже в списке обхода");
                            }
                            data.settings.routing.applications.push(ApplicationRule {
                                id: Uuid::new_v4().to_string(),
                                name: name.chars().take(200).collect(),
                                processes: vec![matcher],
                                source: ApplicationSource::Manual,
                                bypass: true,
                            });
                            Ok(())
                        })?;
                        self.reconnect_if_needed(was_connected)?;
                    }
                    Action::ToggleBypass { id, enabled } => {
                        self.change(|data| {
                            data.settings
                                .routing
                                .applications
                                .iter_mut()
                                .find(|r| r.id == id)
                                .context("Правило удалено")?
                                .bypass = enabled;
                            Ok(())
                        })?;
                        self.reconnect_if_needed(was_connected)?;
                    }
                    Action::DeleteBypass { id } => {
                        self.change(|data| {
                            data.settings.routing.applications.retain(|r| r.id != id);
                            Ok(())
                        })?;
                        self.reconnect_if_needed(was_connected)?;
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(self.snapshot())
    }

    pub fn install_update(&self, progress: impl Fn(app_updater::AppUpdateProgress)) -> Result<()> {
        let release = self
            .update
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .context("Сначала проверьте обновления")?;
        let file = app_updater::download_update(&release, &self.paths.cache_dir, progress)?;
        let _operation = self
            .changes
            .lock()
            .map_err(|_| anyhow::anyhow!("Ошибка завершения подключения"))?;
        self.want_connection.store(false, Ordering::Release);
        self.core.disconnect()?;
        app_updater::launch_update(&file)?;
        Ok(())
    }
}

fn repair_selection(data: &mut AppData) {
    if data
        .selected_subscription
        .is_some_and(|id| !data.subscriptions.iter().any(|s| s.id == id))
    {
        data.selected_subscription = data.subscriptions.first().map(|s| s.id);
    }
    if !data
        .profiles
        .iter()
        .any(|p| Some(p.id) == data.selected_profile)
    {
        data.selected_profile = data
            .profiles
            .iter()
            .find(|p| p.subscription_id == data.selected_subscription)
            .map(|p| p.id);
    }
}

pub fn validate_settings(s: &Settings) -> Result<()> {
    crate::latency::validate_url(&s.ping_url)?;
    for url in [&s.geosite_url, &s.geoip_url] {
        if !url.trim().is_empty() {
            crate::geodata::validate_url(url.trim())?;
        }
    }
    if s.socks_port < 1024 || s.api_port < 1024 || s.socks_port == s.api_port {
        bail!("Порты SOCKS и API должны различаться и быть не ниже 1024");
    }
    if !(1280..=9000).contains(&s.mtu) {
        bail!("MTU должен быть от 1280 до 9000");
    }
    if s.tun_interface_name.is_empty()
        || s.tun_interface_name.len() > 15
        || s.tun_interface_name
            .contains(['\0', '\n', '\r', '/', '\\', ' '])
    {
        bail!("Некорректное имя TUN-интерфейса");
    }
    if !(1..=10).contains(&s.ping_timeout_seconds)
        || !(1..=16).contains(&s.ping_parallelism)
        || !(1..=30).contains(&s.traffic_refresh_seconds)
    {
        bail!("Проверьте интервалы пинга и статистики");
    }
    if s.routing.applications.len() > 2048 {
        bail!("Слишком много правил обхода");
    }
    if !(1..=120).contains(&s.reconnect_delay_seconds)
        || !(1..=168).contains(&s.subscription_update_interval_hours)
        || !(1..=128).contains(&s.mux_concurrency)
    {
        bail!("Проверьте интервалы обновления, переподключения и количество потоков Mux");
    }
    for rule in &s.routing.applications {
        if rule.processes.len() > 256
            || rule
                .processes
                .iter()
                .any(|p| p.is_empty() || p.len() > 4096 || p.contains(['\0', '\n', '\r']))
        {
            bail!("Некорректное правило обхода");
        }
    }
    Ok(())
}

fn ping_targets(data: &AppData, subscription_id: Option<Uuid>) -> Result<Vec<Profile>> {
    if subscription_id.is_some_and(|id| !data.subscriptions.iter().any(|s| s.id == id)) {
        bail!("Подписка удалена");
    }
    let profiles: Vec<_> = data
        .profiles
        .iter()
        .filter(|p| p.subscription_id == subscription_id)
        .cloned()
        .collect();
    if profiles.is_empty() {
        bail!("В выбранной подписке нет серверов для проверки");
    }
    Ok(profiles)
}

#[cfg(target_os = "windows")]
fn configure_autostart(enabled: bool) -> Result<()> {
    crate::windows::configure_autostart(enabled)
}
#[cfg(target_os = "linux")]
fn configure_autostart(enabled: bool) -> Result<()> {
    let base = directories::BaseDirs::new().context("Не найден домашний каталог")?;
    let directory = base.config_dir().join("autostart");
    let path = directory.join("io.nory.NORY.desktop");
    if !enabled {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(directory)?;
    let executable = std::env::current_exe()?
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=NORY\nExec=\"{executable}\"\nIcon=io.nory.NORY\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn flag_image(_: &str) -> Option<String> {
    None
}
#[cfg(target_os = "windows")]
fn flag_image(name: &str) -> Option<String> {
    use base64::Engine;
    let chars: Vec<_> = name.chars().collect();
    let code = if let Some(pair) = chars
        .windows(2)
        .find(|p| p.iter().all(|c| ('🇦'..='🇿').contains(c)))
    {
        format!(
            "{}{}",
            char::from_u32(pair[0] as u32 - '🇦' as u32 + 'A' as u32)?,
            char::from_u32(pair[1] as u32 - '🇦' as u32 + 'A' as u32)?,
        )
    } else {
        let normalized = name.to_lowercase();
        let code = [
            ("париж", "FR"),
            ("paris", "FR"),
            ("амстердам", "NL"),
            ("amsterdam", "NL"),
            ("франкфурт", "DE"),
            ("frankfurt", "DE"),
            ("милан", "IT"),
            ("milan", "IT"),
            ("тирана", "AL"),
            ("tirana", "AL"),
            ("таллин", "EE"),
            ("tallinn", "EE"),
            ("хельсинки", "FI"),
            ("helsinki", "FI"),
            ("варшава", "PL"),
            ("warsaw", "PL"),
            ("шарлотт", "US"),
            ("charlotte", "US"),
            ("москва", "RU"),
            ("moscow", "RU"),
            ("стокгольм", "SE"),
            ("stockholm", "SE"),
            ("лондон", "GB"),
            ("london", "GB"),
            ("прага", "CZ"),
            ("prague", "CZ"),
            ("вена", "AT"),
            ("vienna", "AT"),
            ("цюрих", "CH"),
            ("zurich", "CH"),
            ("токио", "JP"),
            ("tokyo", "JP"),
            ("сингапур", "SG"),
            ("singapore", "SG"),
            ("нью-йорк", "US"),
            ("new york", "US"),
        ]
        .iter()
        .find(|(city, _)| normalized.contains(city))
        .map(|(_, code)| *code)?;
        code.to_string()
    };
    crate::flag_assets::bundled_country_flag(&code).map(|bytes| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    })
}
