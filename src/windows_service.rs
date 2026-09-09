//! LocalSystem TUN broker. Its executable, cores and runtime directories must
//! be administrator-owned (created by the installer), never user-writable.
//! RPC accepts configurations, not commands or executable/configuration paths.
use crate::{
    privileged,
    process::hide_window,
    windows::{self, OwnedHandle, ProcessJob, wide},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, SECURITY_ATTRIBUTES},
    Storage::FileSystem::*,
    System::{Pipes::*, Services::*, Threading::*},
};

const PIPE: &str = r"\\.\pipe\NORY.Tunnel.v1";
const SERVICE: &str = "NoryTunnel";
const MAX_MESSAGE: usize = 2 * 1024 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
static RPC: Mutex<()> = Mutex::new(());
static STOPPING: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
pub(crate) enum Request {
    Ping,
    TunnelStatus,
    Stop,
    CheckXrayAccess,
    AuthorizeXray {
        owner_pid: u32,
    },
    StartTun {
        name: String,
        enable_ipv6: bool,
        auto_route: bool,
        mtu: u16,
        socks_port: u16,
        bypass_processes: Vec<String>,
    },
    StartMihomo {
        config: Value,
    },
}

#[derive(Serialize, Deserialize)]
struct Reply {
    error: Option<String>,
    #[serde(default)]
    interface: Option<String>,
}

pub(crate) fn call(request: &Request) -> Result<()> {
    call_reply(request).map(|_| ())
}

pub(crate) fn start_tunnel(request: &Request) -> Result<String> {
    call_reply(request)?.interface.context(
        "Служба NORY не вернула имя TUN. Повторите установку обновления, чтобы обновить системную службу",
    )
}

fn call_reply(request: &Request) -> Result<Reply> {
    call_reply_with_timeout(request, RPC_TIMEOUT)
}

pub(crate) fn tunnel_alive(name: &str) -> Result<bool> {
    let reply = call_reply_with_timeout(&Request::TunnelStatus, Duration::from_secs(1))?;
    Ok(reply.interface.as_deref() == Some(name))
}

fn call_reply_with_timeout(request: &Request, timeout: Duration) -> Result<Reply> {
    let _lock = RPC
        .lock()
        .map_err(|_| anyhow::anyhow!("Состояние службы NORY повреждено"))?;
    let deadline = Instant::now() + timeout;
    let mut pipe = loop {
        match OpenOptions::new()
            .access_mode(FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES)
            .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(PIPE)
        {
            Ok(pipe) => break pipe,
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(error) => return Err(error).context(
                "Служба NoryTunnel недоступна. Установите NORY через установщик и запустите службу",
            ),
        }
    };
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server_pid) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    verify_service_pid(server_pid)?;
    nonblocking(&pipe)?;
    write_message(&mut pipe, &serde_json::to_vec(request)?, deadline)?;
    let reply: Reply = serde_json::from_slice(&read_message(&mut pipe, deadline)?)?;
    if let Some(error) = &reply.error {
        bail!("{error}");
    }
    Ok(reply)
}

fn nonblocking(pipe: &File) -> Result<()> {
    let mode = PIPE_READMODE_MESSAGE | PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(pipe.as_raw_handle(), &mode, ptr::null(), ptr::null()) }
        == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn verify_service_pid(pid: u32) -> Result<()> {
    // SCM state is readable without opening a LocalSystem process handle.
    // Users cannot register/replace the service, unlike a pipe name alone.
    let manager = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let service = unsafe { OpenServiceW(manager, wide(SERVICE).as_ptr(), SERVICE_QUERY_STATUS) };
    unsafe { CloseServiceHandle(manager) };
    if service.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    let success = unsafe {
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of_val(&status) as u32,
            &mut needed,
        )
    };
    unsafe { CloseServiceHandle(service) };
    if success == 0 || status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId != pid {
        bail!("Канал NORY не принадлежит установленной службе NoryTunnel");
    }
    Ok(())
}

fn read_message(pipe: &mut File, deadline: Instant) -> Result<Vec<u8>> {
    let mut bytes = vec![0; MAX_MESSAGE];
    loop {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("Канал службы NORY закрыт");
        }
        if available as usize > MAX_MESSAGE {
            bail!("Слишком большое сообщение службы NORY");
        }
        if available > 0 {
            match pipe.read(&mut bytes) {
                Ok(0) => {}
                Ok(size) => {
                    bytes.truncate(size);
                    return Ok(bytes);
                }
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {}
                Err(error) => return Err(error).context("Не удалось прочитать ответ службы NORY"),
            }
        }
        if Instant::now() >= deadline || STOPPING.load(Ordering::Acquire) {
            bail!("Служба NORY не ответила вовремя");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_message(pipe: &mut File, bytes: &[u8], deadline: Instant) -> Result<()> {
    if bytes.len() > MAX_MESSAGE {
        bail!("Конфигурация NORY больше 2 МиБ");
    }
    loop {
        match pipe.write(bytes) {
            Ok(size) if size == bytes.len() => return Ok(()),
            Ok(0) => {}
            Ok(_) => bail!("Неполное сообщение службы NORY"),
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline || STOPPING.load(Ordering::Acquire) {
            bail!("Таймаут службы NORY");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn image_path(handle: HANDLE) -> Result<PathBuf> {
    let mut bytes = vec![0u16; 32768];
    let mut length = bytes.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, bytes.as_mut_ptr(), &mut length) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_wide(&bytes[..length as usize]).into())
}

fn install_directory() -> Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("Нет каталога установки NORY")?
        .to_path_buf())
}

fn verify_peer(pid: u32, executable: &str) -> Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let handle = OwnedHandle(handle);
    let expected = fs::canonicalize(install_directory()?.join(executable))?;
    let actual = fs::canonicalize(image_path(handle.0)?)?;
    if !actual
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
    {
        bail!("Неизвестный клиент или служба NORY");
    }
    Ok(handle)
}

fn process_is_elevated(process: HANDLE) -> Result<bool> {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Не удалось проверить права процесса");
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut needed = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of_val(&elevation) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("Не удалось проверить токен администратора");
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// Only invoked by the runas helper; the service checks the token independently.
pub(crate) fn authorize_xray(owner_pid: u32) -> Result<()> {
    if !process_is_elevated(unsafe { GetCurrentProcess() })? {
        bail!("Xray TUN требует подтверждения администратора Windows");
    }
    call(&Request::AuthorizeXray { owner_pid })
}

struct XrayConsent {
    owner: OwnedHandle,
    owner_pid: u32,
}

impl XrayConsent {
    fn allows(&self, pid: u32) -> bool {
        // Holding the original process object prevents PID reuse from carrying
        // consent over to another instance. No permission is persisted on disk.
        self.owner_pid == pid && unsafe { WaitForSingleObject(self.owner.0, 0) } == WAIT_TIMEOUT
    }
}

fn require_xray_access(consent: &Option<XrayConsent>, pid: u32, owner: HANDLE) -> Result<()> {
    if consent.as_ref().is_some_and(|consent| consent.allows(pid)) || process_is_elevated(owner)? {
        return Ok(());
    }
    bail!("Xray TUN требует прав администратора. Подтвердите запрос Windows при подключении")
}

fn pipe_server() -> Result<File> {
    let mut descriptor = ptr::null_mut();
    // Interactive users can communicate, but cannot create another instance.
    // The executable path is checked independently for every connection.
    let sddl = wide("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012019b;;;IU)");
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let handle = unsafe {
        CreateNamedPipeW(
            wide(PIPE).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            MAX_MESSAGE as u32,
            MAX_MESSAGE as u32,
            0,
            &attributes,
        )
    };
    unsafe { LocalFree(descriptor) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(crate) fn run() -> Result<()> {
    windows::require_windows_11()?;
    let mut name = wide(SERVICE);
    let entries = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(entries.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Only the elevated installer invokes this command. GUI requests cannot
/// change service registration, ACLs or the core executable locations.
pub(crate) fn install() -> Result<()> {
    use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
    windows::require_windows_11()?;
    if unsafe { IsUserAnAdmin() } == 0 {
        bail!("Установку службы должен запускать администратор");
    }
    let directory = install_directory()?;
    let expected = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .context("Нет Program Files")?
        .join("NORY");
    if !fs::canonicalize(&directory)?
        .to_string_lossy()
        .eq_ignore_ascii_case(&fs::canonicalize(&expected)?.to_string_lossy())
    {
        bail!("Службу можно устанавливать только из Program Files\\NORY");
    }
    require_plain_directory(&directory)?;
    #[cfg(feature = "native-ui")]
    prepare_graphics_runtime(&directory)?;
    let runtime = crate::storage::Paths::system().runtime_dir;
    let base = runtime.parent().context("Нет системного каталога NORY")?;
    fs::create_dir_all(base)?;
    require_plain_directory(base)?;
    protect_directory(base)?;
    fs::create_dir_all(&runtime)?;
    require_plain_directory(&runtime)?;
    protect_directory(&runtime)?;
    // Do not reuse possible old links/files from a pre-service installation.
    for name in ["active.json", "core.log", "GeoIP.dat", "GeoSite.dat"] {
        let file = runtime.join(name);
        if fs::symlink_metadata(&file).is_ok() {
            fs::remove_file(file)?;
        }
    }
    struct ServiceHandle(SC_HANDLE);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe { CloseServiceHandle(self.0) };
        }
    }
    let manager = unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
        )
    };
    if manager.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let manager = ServiceHandle(manager);
    let binary = wide(format!(
        "\"{}\" service",
        directory.join("nory-helper.exe").display()
    ));
    let access = SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_QUERY_STATUS;
    let mut service = unsafe { OpenServiceW(manager.0, wide(SERVICE).as_ptr(), access) };
    if service.is_null() {
        service = unsafe {
            CreateServiceW(
                manager.0,
                wide(SERVICE).as_ptr(),
                wide("NORY Tunnel").as_ptr(),
                access,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                binary.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        };
        if service.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
    } else if unsafe {
        ChangeServiceConfigW(
            service,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            binary.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            wide("LocalSystem").as_ptr(),
            ptr::null(),
            wide("NORY Tunnel").as_ptr(),
        )
    } == 0
    {
        unsafe { CloseServiceHandle(service) };
        return Err(std::io::Error::last_os_error().into());
    }
    let service = ServiceHandle(service);
    let mut description =
        wide("NORY: управляет одним TUN-ядром, не запускает VPN без запроса приложения.");
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: description.as_mut_ptr(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            (&description as *const SERVICE_DESCRIPTIONW).cast(),
        )
    };
    if unsafe { StartServiceW(service.0, 0, ptr::null()) } == 0
        && unsafe { GetLastError() } != ERROR_SERVICE_ALREADY_RUNNING
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut status = SERVICE_STATUS::default();
    while Instant::now() < deadline {
        if unsafe { QueryServiceStatus(service.0, &mut status) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        if status.dwCurrentState == SERVICE_STOPPED {
            bail!(
                "Служба NORY остановилась при запуске (Windows {})",
                status.dwWin32ExitCode
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("Служба NORY не запустилась за 10 секунд")
}

fn protect_directory(path: &Path) -> Result<()> {
    use windows_sys::Win32::Security::*;
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)").as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut acl = ptr::null_mut();
    let mut owner = ptr::null_mut();
    let (mut present, mut defaulted) = (0, 0);
    let valid =
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            != 0
            && unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } != 0;
    let error = if valid {
        unsafe {
            SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                ptr::null_mut(),
                acl,
                ptr::null(),
            )
        }
    } else {
        ERROR_INVALID_SECURITY_DESCR
    };
    unsafe { LocalFree(descriptor) };
    if error != 0 {
        bail!("Не удалось защитить системный каталог NORY (Windows {error})");
    }
    Ok(())
}

fn prepare_graphics_runtime(directory: &Path) -> Result<()> {
    let schemas = directory.join("share/glib-2.0/schemas");
    let mut compile = Command::new(directory.join("glib-compile-schemas.exe"));
    compile
        .arg(&schemas)
        .current_dir(directory)
        .stdin(Stdio::null());
    hide_window(&mut compile);
    let result = crate::core::command_output_with_timeout(compile, Duration::from_secs(15))?;
    if !result.status.success() {
        bail!("Не удалось подготовить GSettings. Переустановите NORY");
    }
    let loaders = directory.join("lib/gdk-pixbuf-2.0/2.10.0/loaders");
    let mut query = Command::new(directory.join("gdk-pixbuf-query-loaders.exe"));
    query
        .env("GDK_PIXBUF_MODULEDIR", &loaders)
        .current_dir(directory)
        .stdin(Stdio::null());
    hide_window(&mut query);
    let result = crate::core::command_output_with_timeout(query, Duration::from_secs(15))?;
    if !result.status.success() || result.stdout.is_empty() {
        bail!("Не удалось подготовить загрузчики значков GTK");
    }
    fs::write(
        directory.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"),
        &result.stdout,
    )?;
    Ok(())
}

fn report(state: u32, error: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_STOP_PENDING {
            15000
        } else {
            0
        },
    };
    unsafe {
        SetServiceStatus(
            STATUS_HANDLE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE,
            &status,
        )
    };
}

unsafe extern "system" fn service_control(
    control: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        STOPPING.store(true, Ordering::Release);
        report(SERVICE_STOP_PENDING, 0);
    }
    NO_ERROR
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(wide(SERVICE).as_ptr(), Some(service_control), ptr::null())
    };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::Release);
    let result = serve();
    report(
        SERVICE_STOPPED,
        if result.is_ok() {
            0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
    );
}

fn serve() -> Result<()> {
    let directory = install_directory()?;
    // The installer protects this tree before starting the service. Refuse
    // junctions/symlinks, including its ancestors; never follow user paths.
    let runtime = crate::storage::Paths::system().runtime_dir;
    require_plain_directory(&runtime)?;
    let mut pipe = pipe_server()?;
    let mut session: Option<Session> = None;
    let mut xray_consent: Option<XrayConsent> = None;
    report(SERVICE_RUNNING, 0);
    while !STOPPING.load(Ordering::Acquire) {
        if xray_consent
            .as_ref()
            .is_some_and(|consent| !consent.allows(consent.owner_pid))
        {
            xray_consent.take();
        }
        if session.as_mut().is_some_and(|session| session.finished()) {
            // Retain ownership if cleanup fails, so Stop can retry it.
            let _ = stop_session(&mut session);
        }
        let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), ptr::null_mut()) } != 0;
        let error = unsafe { GetLastError() };
        if !connected && error != ERROR_PIPE_CONNECTED {
            if error == ERROR_NO_DATA {
                unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
            }
            if !matches!(error, ERROR_PIPE_LISTENING | ERROR_NO_DATA) {
                return Err(std::io::Error::from_raw_os_error(error as i32).into());
            }
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        let result = (|| -> Result<()> {
            let mut pid = 0;
            if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // Both endpoints are pinned to protected installed executables.
            // Only an elevated helper may grant access; only the GUI may start
            // or stop a tunnel. No executable paths arrive over RPC.
            let (owner, is_helper) = match verify_peer(pid, "nory.exe") {
                Ok(owner) => (owner, false),
                Err(_) => (verify_peer(pid, "nory-helper.exe")?, true),
            };
            let request: Request = serde_json::from_slice(&read_message(
                &mut pipe,
                Instant::now() + Duration::from_secs(3),
            )?)?;
            if let Request::AuthorizeXray { owner_pid } = request {
                if !is_helper || !process_is_elevated(owner.0)? {
                    bail!("Авторизацию Xray TUN должен подтвердить администратор Windows");
                }
                if session.as_ref().is_some_and(|s| s.owner_pid != owner_pid) {
                    bail!("VPN уже запущен в другом сеансе NORY");
                }
                let client = verify_peer(owner_pid, "nory.exe")?;
                let consent = XrayConsent {
                    owner: client,
                    owner_pid,
                };
                if !consent.allows(owner_pid) {
                    bail!("Приложение NORY уже закрыто");
                }
                xray_consent = Some(consent);
                return Ok(());
            }
            if is_helper {
                bail!("Эта команда доступна только интерфейсу NORY");
            }
            if matches!(request, Request::Ping) {
                return Ok(());
            }
            if matches!(request, Request::CheckXrayAccess) {
                return require_xray_access(&xray_consent, pid, owner.0);
            }
            if session.as_ref().is_some_and(|s| s.owner_pid != pid) {
                bail!("VPN уже запущен в другом сеансе NORY. Сначала отключите его там");
            }
            match request {
                Request::Ping => {}
                Request::TunnelStatus => {
                    if session.as_mut().is_some_and(|s| s.finished()) {
                        stop_session(&mut session)?;
                    }
                }
                Request::CheckXrayAccess | Request::AuthorizeXray { .. } => unreachable!(),
                Request::Stop => {
                    stop_session(&mut session)?;
                }
                Request::StartTun {
                    name,
                    enable_ipv6,
                    auto_route,
                    mtu,
                    socks_port,
                    bypass_processes,
                } => {
                    // Enforced here, not only by the GUI: a direct StartTun RPC
                    // cannot skip UAC. Check before replacing an active core.
                    require_xray_access(&xray_consent, pid, owner.0)?;
                    let mut config = privileged::sing_box_config(
                        &name,
                        enable_ipv6,
                        auto_route,
                        mtu,
                        socks_port,
                        &bypass_processes,
                    )?;
                    stop_session(&mut session)?;
                    let name = windows::select_tun_interface(&name)?;
                    config["inbounds"][0]["interface_name"] = json!(&name);
                    session = Some(Session::start(
                        &directory, &runtime, false, &name, &config, owner, pid,
                    )?);
                }
                Request::StartMihomo { config } => {
                    let mut config = secure_mihomo(config)?;
                    let preferred = config["tun"]["device"].as_str().context("Нет имени TUN")?;
                    stop_session(&mut session)?;
                    let name = windows::select_tun_interface(preferred)?;
                    config["tun"]["device"] = json!(&name);
                    session = Some(Session::start(
                        &directory, &runtime, true, &name, &config, owner, pid,
                    )?);
                }
            }
            Ok(())
        })();
        let reply = Reply {
            interface: result
                .is_ok()
                .then(|| session.as_ref().map(|s| s.interface.clone()))
                .flatten(),
            error: result
                .err()
                .map(|e| format!("{e:#}").chars().take(2000).collect()),
        };
        let _ = write_message(
            &mut pipe,
            &serde_json::to_vec(&reply)?,
            Instant::now() + Duration::from_secs(3),
        );
        // Wait for the peer to consume the buffered response, never flush a
        // synchronous pipe (a stalled client would hang service shutdown).
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && !STOPPING.load(Ordering::Acquire) {
            if unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } == 0
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
    }
    drop(session);
    Ok(())
}

fn require_plain_directory(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    for part in path.ancestors().filter(|p| p.parent().is_some()) {
        let metadata = fs::symlink_metadata(part)
            .context("Системный каталог NORY отсутствует. Переустановите приложение")?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("Небезопасный системный каталог NORY");
        }
    }
    Ok(())
}

struct Session {
    child: Child,
    _job: ProcessJob,
    owner: OwnedHandle,
    owner_pid: u32,
    interface: String,
    luid: Option<u64>,
    config_path: PathBuf,
}

fn stop_session(session: &mut Option<Session>) -> Result<()> {
    if let Some(running) = session.as_mut() {
        running.stop()?;
    }
    session.take();
    Ok(())
}

impl Session {
    fn start(
        directory: &Path,
        runtime: &Path,
        mihomo: bool,
        interface: &str,
        config: &Value,
        owner: OwnedHandle,
        owner_pid: u32,
    ) -> Result<Self> {
        // A registered, disconnected Wintun adapter is not a running tunnel.
        // Let the core open/reuse it; only an active or foreign device conflicts.
        windows::require_available_tun(interface)?;
        let core = directory
            .join("cores")
            .join(if mihomo { "mihomo" } else { "sing-box" });
        require_plain_directory(&core)?;
        let binary = core.join(if mihomo { "mihomo.exe" } else { "sing-box.exe" });
        let config_path = runtime.join("active.json");
        crate::storage::atomic_json(&config_path, config)?;
        if mihomo {
            for (source, target) in [("geoip.dat", "GeoIP.dat"), ("geosite.dat", "GeoSite.dat")] {
                fs::copy(
                    directory.join("cores/xray").join(source),
                    runtime.join(target),
                )
                .with_context(|| format!("Не удалось подготовить {source}"))?;
            }
        }
        let command = |validate: bool| {
            let mut command = Command::new(&binary);
            if mihomo {
                if validate {
                    command.arg("-t");
                }
                command.arg("-d").arg(runtime).arg("-f").arg(&config_path);
            } else {
                command
                    .arg(if validate { "check" } else { "run" })
                    .arg("-c")
                    .arg(&config_path)
                    .arg("--disable-color");
            }
            command.current_dir(&core).stdin(Stdio::null());
            hide_window(&mut command);
            command
        };
        let check =
            crate::core::command_output_with_timeout(command(true), Duration::from_secs(10))?;
        if !check.status.success() {
            // Keep full diagnostics in an administrator-only log, not in an
            // IPC reply: configs may contain subscription credentials.
            fs::write(
                runtime.join("core.log"),
                [check.stdout, check.stderr].concat(),
            )?;
            bail!(
                "Ядро отклонило конфигурацию. Подробности: C:\\ProgramData\\NORY\\runtime\\core.log"
            );
        }
        let log = File::create(runtime.join("core.log"))?;
        let job = ProcessJob::new()?;
        let mut child = command(false)
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("Не удалось запустить TUN-ядро")?;
        if let Err(error) = job.attach(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let mut session = Self {
            child,
            _job: job,
            owner,
            owner_pid,
            interface: interface.into(),
            luid: None,
            config_path,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !STOPPING.load(Ordering::Acquire) {
            if session.finished() {
                bail!(
                    "TUN-ядро завершилось при запуске. Подробности: C:\\ProgramData\\NORY\\runtime\\core.log"
                );
            }
            if let Ok(row) = windows::interface_row(interface) {
                session.luid = Some(unsafe { row.InterfaceLuid.Value });
                // Wintun is a layer-3 adapter. Windows may report its media
                // state as Down/Unknown while sing-box already owns the
                // Wintun session and is processing packets. Requiring
                // OperStatus=Up here incorrectly killed a valid tunnel after
                // the interface had been created.
                return Ok(session);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("Ошибка подключения: TUN не создан. Проверьте драйвер Wintun или попробуйте ещё раз")
    }

    fn finished(&mut self) -> bool {
        self.child
            .try_wait()
            .map_or(true, |status| status.is_some())
            || unsafe { WaitForSingleObject(self.owner.0, 0) } != WAIT_TIMEOUT
    }

    fn stop(&mut self) -> Result<()> {
        if self.luid.is_none() {
            self.luid = windows::interface_row(&self.interface)
                .ok()
                .map(|row| unsafe { row.InterfaceLuid.Value });
        }
        self._job.terminate()?;
        if unsafe { WaitForSingleObject(self.child.as_raw_handle(), 5_000) } != WAIT_OBJECT_0 {
            bail!("TUN-ядро ещё не остановилось. Повторите отключение");
        }
        self.child
            .wait()
            .context("Не удалось подтвердить остановку TUN-ядра")?;
        if let Some(luid) = self.luid {
            remove_owned_routes(luid)?;
        }
        match fs::remove_file(&self.config_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("Не удалось удалить временную конфигурацию TUN");
            }
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn remove_owned_routes(luid: u64) -> Result<()> {
    use windows_sys::Win32::{NetworkManagement::IpHelper::*, Networking::WinSock::AF_UNSPEC};
    let mut table: *mut MIB_IPFORWARD_TABLE2 = ptr::null_mut();
    let error = unsafe { GetIpForwardTable2(AF_UNSPEC, &mut table) };
    if error != 0 || table.is_null() {
        bail!("Не удалось проверить маршруты TUN (Windows {error})");
    }
    let mut first_error = 0;
    unsafe {
        for row in std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        {
            if row.InterfaceLuid.Value == luid {
                let error = DeleteIpForwardEntry2(row);
                if error != 0 && error != ERROR_NOT_FOUND && first_error == 0 {
                    first_error = error;
                }
            }
        }
        FreeMibTable(table.cast());
    }
    if first_error != 0 {
        bail!("Не удалось удалить маршрут TUN (Windows {first_error}). Повторите отключение");
    }
    Ok(())
}

fn secure_mihomo(mut config: Value) -> Result<Value> {
    let object = config
        .as_object_mut()
        .context("Конфигурация Mihomo должна быть объектом")?;
    const ALLOWED: &[&str] = &[
        "mode",
        "log-level",
        "ipv6",
        "allow-lan",
        "find-process-mode",
        "unified-delay",
        "tcp-concurrent",
        "external-controller",
        "profile",
        "dns",
        "tun",
        "hosts",
        "sniffer",
        "geodata-mode",
        "geo-auto-update",
        "global-client-fingerprint",
        "keep-alive-interval",
        "keep-alive-idle",
        "disable-keep-alive",
        "proxies",
        "proxy-groups",
        "rules",
        "sub-rules",
    ];
    if let Some(key) = object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
        bail!(
            "Параметр Mihomo «{key}» не поддерживается системной службой. Используйте конфигурацию с встроенными узлами, без внешних файлов и providers"
        );
    }
    let interface = object
        .get("tun")
        .and_then(|tun| tun["device"].as_str())
        .context("Нет имени TUN")?
        .to_string();
    if !privileged::valid_interface(&interface) {
        bail!("Некорректное имя TUN");
    }
    let mtu = object["tun"]["mtu"].as_u64().unwrap_or(1500);
    if !(1280..=9000).contains(&mtu) {
        bail!("Некорректный MTU");
    }
    object.insert(
        "tun".into(),
        json!({ "enable": true, "device": interface, "stack": "mixed", "auto-route": true,
        "auto-redirect": false, "strict-route": true, "auto-detect-interface": true, "mtu": mtu,
        "dns-hijack": ["any:53", "tcp://any:53"] }),
    );
    object.insert("external-controller".into(), json!(""));
    object.insert("allow-lan".into(), json!(false));
    object.insert("geo-auto-update".into(), json!(false));
    object.insert(
        "profile".into(),
        json!({"store-selected": false, "store-fake-ip": false}),
    );
    object.insert("log-level".into(), json!("warning"));
    if let Some(dns) = object.get_mut("dns").and_then(Value::as_object_mut) {
        dns.insert("listen".into(), json!("127.0.0.1:0"));
    }
    reject_file_options(&config, 0)?;
    Ok(config)
}

fn reject_file_options(value: &Value, depth: usize) -> Result<()> {
    if depth > 64 {
        bail!("Слишком сложная конфигурация Mihomo");
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "private-key-path"
                        | "certificate-path"
                        | "ca"
                        | "ca-str"
                        | "client-key"
                        | "client-certificate"
                        | "file"
                        | "exec"
                        | "script"
                ) {
                    bail!("Внешние файлы и команды не разрешены в системном ядре ({key})");
                }
                // WebSocket/HTTP paths are valid; paths to local files are not.
                if key == "path"
                    && value
                        .as_str()
                        .is_some_and(|v| v.contains('\\') || v.contains(':') || v.contains(".."))
                {
                    bail!("Локальные пути не разрешены в конфигурации Mihomo");
                }
                reject_file_options(value, depth + 1)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                reject_file_options(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
