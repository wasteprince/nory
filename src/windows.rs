//! Windows 11 integration. No localized shell output is used for system state.
use anyhow::{Context, Result, bail};
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
    System::{JobObjects::*, SystemInformation::OSVERSIONINFOEXW},
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
};

pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub(crate) struct OwnedHandle(pub HANDLE);
// A kernel handle may be transferred between threads; it has one owner.
unsafe impl Send for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn version_info() -> Result<OSVERSIONINFOEXW> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOEXW) -> i32;
    }
    let mut info = OSVERSIONINFOEXW::default();
    info.dwOSVersionInfoSize = std::mem::size_of_val(&info) as u32;
    // RtlGetVersion is independent of cmd.exe encoding and compatibility shims.
    if unsafe { RtlGetVersion(&mut info) } < 0 {
        bail!("Не удалось определить версию Windows");
    }
    Ok(info)
}

pub fn os_version_description() -> String {
    version_info()
        .map(|info| format!("Windows 11 build {}", info.dwBuildNumber))
        .unwrap_or_else(|_| "Windows 11".into())
}

pub fn require_windows_11() -> Result<()> {
    use windows_sys::Win32::System::SystemInformation::{
        GetNativeSystemInfo, PROCESSOR_ARCHITECTURE_AMD64, SYSTEM_INFO,
    };
    let info = version_info()?;
    let mut system = SYSTEM_INFO::default();
    unsafe { GetNativeSystemInfo(&mut system) };
    if info.dwMajorVersion != 10
        || info.dwBuildNumber < 22000
        || info.wProductType != 1
        || unsafe { system.Anonymous.Anonymous.wProcessorArchitecture }
            != PROCESSOR_ARCHITECTURE_AMD64
    {
        bail!(
            "NORY поддерживает только Windows 11 x64 (сборка 22000 и новее), не Windows 10 или Windows Server"
        );
    }
    Ok(())
}

pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            wide(message).as_ptr(),
            wide("NORY").as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn configure_process() -> Result<()> {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    unsafe { SetCurrentProcessExplicitAppUserModelID(wide("io.nory.NORY").as_ptr()) };
    let directory = std::env::current_exe()?
        .parent()
        .context("каталог NORY недоступен")?
        .to_path_buf();
    // Called before GTK or any worker starts. Use only our bundled runtime;
    // third-party GTK installations must not supply incompatible schemas/DLLs.
    unsafe {
        std::env::set_var(
            "GSETTINGS_SCHEMA_DIR",
            directory.join("share/glib-2.0/schemas"),
        );
        std::env::set_var("XDG_DATA_DIRS", directory.join("share"));
        std::env::set_var("GTK_DATA_PREFIX", &directory);
        std::env::set_var("GSETTINGS_BACKEND", "memory");
        std::env::set_var("GDK_BACKEND", "win32");
        std::env::set_var(
            "GDK_PIXBUF_MODULE_FILE",
            directory.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"),
        );
        std::env::set_var(
            "GDK_PIXBUF_MODULEDIR",
            directory.join("lib/gdk-pixbuf-2.0/2.10.0/loaders"),
        );
        // Let GTK select an accelerated renderer, falling back to Cairo when
        // necessary. Respect an explicit GSK_RENDERER troubleshooting override.
        // Forcing Cairo here disabled GTK's normal high-DPI rendering path.
    }
    Ok(())
}

pub fn interface_row(name: &str) -> Result<MIB_IF_ROW2> {
    let mut row = MIB_IF_ROW2::default();
    let result =
        unsafe { ConvertInterfaceAliasToLuid(wide(name).as_ptr(), &mut row.InterfaceLuid) };
    if result != 0 {
        bail!("TUN-интерфейс не найден (Windows {result})");
    }
    let result = unsafe { GetIfEntry2(&mut row) };
    if result != 0 {
        bail!("Не удалось прочитать состояние TUN (Windows {result})");
    }
    Ok(row)
}

/// Enumerate rather than treating every lookup error as "no adapter". This is
/// read-only and never creates, enables, deletes or renames a network device.
pub(crate) fn existing_interface(name: &str) -> Result<Option<MIB_IF_ROW2>> {
    let mut table = ptr::null_mut();
    let error = unsafe { GetIfTable2(&mut table) };
    if error != 0 || table.is_null() {
        bail!("Не удалось прочитать сетевые адаптеры Windows ({error})");
    }
    let target = name.to_lowercase();
    let row = unsafe {
        std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
            .iter()
            .find(|row| utf16_text(&row.Alias).to_lowercase() == target)
            .copied()
    };
    unsafe { FreeMibTable(table.cast()) };
    Ok(row)
}

fn utf16_text(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())])
}

/// Match the actual installed driver service, not a localized/friendly name.
/// All registry/device handles below are read-only. Only the selected adapter
/// GUID is inspected; inactive Ethernet/Wi-Fi/other VPN adapters are not reused.
fn has_wintun_driver(row: &MIB_IF_ROW2) -> Result<Option<bool>> {
    use crate::windows_tun_state::{luid_matches, netcfg_matches};
    use windows_sys::Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{ERROR_NO_MORE_ITEMS, GetLastError},
        System::Registry::*,
    };
    let devices = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_NET,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT,
        )
    };
    if devices == INVALID_HANDLE_VALUE as HDEVINFO {
        return Err(std::io::Error::last_os_error()).context("Не удалось проверить драйвер TUN");
    }
    struct Devices(HDEVINFO);
    impl Drop for Devices {
        fn drop(&mut self) {
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }
    let devices = Devices(devices);
    let guid = row.InterfaceGuid;
    let target = format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    );
    for index in 0.. {
        let mut device = SP_DEVINFO_DATA::default();
        device.cbSize = std::mem::size_of_val(&device) as u32;
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut device) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                return Ok(None);
            }
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("Не удалось проверить устройство TUN");
        }
        let key = unsafe {
            SetupDiOpenDevRegKey(
                devices.0,
                &device,
                DICS_FLAG_GLOBAL,
                0,
                DIREG_DRV,
                KEY_QUERY_VALUE,
            )
        };
        if key == INVALID_HANDLE_VALUE {
            continue;
        }
        let mut value = [0u16; 64];
        let mut bytes = std::mem::size_of_val(&value) as u32;
        let error = unsafe {
            RegGetValueW(
                key,
                ptr::null(),
                wide("NetCfgInstanceId").as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                value.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        let guid_matches = error == 0 && netcfg_matches(&utf16_text(&value), &target);
        // NDIS also records the exact LUID components in the driver's key.
        // Use this identity when NetCfgInstanceId is missing/not populated yet;
        // never identify a driver by its localized Description or adapter name.
        let read_dword = |name: &str| -> Option<u32> {
            let mut value = 0u32;
            let mut bytes = 4u32;
            let error = unsafe {
                RegGetValueW(
                    key,
                    ptr::null(),
                    wide(name).as_ptr(),
                    RRF_RT_REG_DWORD,
                    ptr::null_mut(),
                    (&mut value as *mut u32).cast(),
                    &mut bytes,
                )
            };
            (error == 0).then_some(value)
        };
        let luid_matches = read_dword("NetLuidIndex")
            .zip(read_dword("*IfType"))
            .is_some_and(|(index, kind)| {
                luid_matches(unsafe { row.InterfaceLuid.Value }, index, kind)
            });
        unsafe { RegCloseKey(key) };
        if !guid_matches && !luid_matches {
            continue;
        }
        let mut service = [0u16; 256];
        let mut kind = 0;
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                &device,
                SPDRP_SERVICE,
                &mut kind,
                service.as_mut_ptr().cast(),
                std::mem::size_of_val(&service) as u32,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("Не удалось прочитать драйвер выбранного адаптера");
        }
        return Ok(Some(
            kind == REG_SZ && utf16_text(&service).eq_ignore_ascii_case("wintun"),
        ));
    }
    unreachable!()
}

fn tun_availability(name: &str) -> Result<crate::windows_tun_state::AdapterAvailability> {
    use crate::windows_tun_state::{AdapterState, availability};
    use windows_sys::Win32::NetworkManagement::Ndis::MediaConnectStateConnected;
    let adapter = existing_interface(name)?
        .map(|row| -> Result<AdapterState> {
            Ok(AdapterState {
                wintun_driver: has_wintun_driver(&row)?,
                operational: row.OperStatus == IfOperStatusUp,
                media_connected: row.MediaConnectState == MediaConnectStateConnected,
            })
        })
        .transpose()?;
    Ok(availability(adapter))
}

pub(crate) fn select_tun_interface(name: &str) -> Result<String> {
    crate::windows_tun_state::select_interface(name, tun_availability)
}

pub(crate) fn require_available_tun(name: &str) -> Result<()> {
    use crate::windows_tun_state::AdapterAvailability;
    match tun_availability(name)? {
        AdapterAvailability::Available => Ok(()),
        AdapterAvailability::Busy => bail!(
            "TUN-интерфейс {name} действительно активен в другом сеансе. Сначала отключите использующий его VPN"
        ),
        AdapterAvailability::Foreign => bail!(
            "Имя {name} принадлежит другому сетевому адаптеру, не Wintun. Укажите другое имя TUN в настройках NORY"
        ),
        AdapterAvailability::Unknown => bail!(
            "Не удалось определить драйвер адаптера {name}. Повторите подключение для выбора свободного имени"
        ),
    }
}

pub fn traffic(name: &str) -> Result<(u64, u64)> {
    let row = interface_row(name)?;
    Ok((row.OutOctets, row.InOctets))
}

/// Close the job on crash/exit to prevent orphaned VPN core processes.
pub(crate) struct ProcessJob(OwnedHandle);
impl ProcessJob {
    pub fn new() -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let handle = OwnedHandle(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(handle))
    }
    pub fn attach(&self, child: &std::process::Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        if unsafe { AssignProcessToJobObject(self.0.0, child.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn terminate(&self) -> Result<()> {
        if unsafe { TerminateJobObject(self.0.0, 1) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("Не удалось остановить процессы TUN");
        }
        Ok(())
    }
}

pub fn open_path(path: &Path) -> Result<()> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            wide("open").as_ptr(),
            wide(path).as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as usize <= 32 {
        bail!("Windows не удалось открыть каталог ({})", result as usize);
    }
    Ok(())
}

pub fn configure_autostart(enabled: bool) -> Result<()> {
    use windows_sys::Win32::System::Registry::*;
    let command = if enabled {
        Some(wide(format!("\"{}\"", std::env::current_exe()?.display())))
    } else {
        None
    };
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Run").as_ptr(),
            0,
            ptr::null(),
            0,
            KEY_SET_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    if result != 0 {
        bail!("Не удалось открыть настройки автозапуска (Windows {result})");
    }
    let result = if let Some(command) = command {
        unsafe {
            RegSetValueExW(
                key,
                wide("NORY").as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteValueW(key, wide("NORY").as_ptr()) }
    };
    unsafe { RegCloseKey(key) };
    if result != 0 && !(result == 2 && !enabled) {
        bail!("Не удалось изменить автозапуск (Windows {result})");
    }
    Ok(())
}

pub struct SingleInstance {
    _mutex: OwnedHandle,
    show: OwnedHandle,
}

impl SingleInstance {
    /// A session-local kernel mutex also works without D-Bus on Windows.
    pub fn acquire() -> Result<Option<Self>> {
        use windows_sys::Win32::{
            Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
            System::Threading::*,
        };
        let mutex =
            unsafe { CreateMutexW(ptr::null(), 0, wide(r"Local\NORY.Desktop.v1").as_ptr()) };
        if mutex.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = OwnedHandle(mutex);
        let show = unsafe { CreateEventW(ptr::null(), 0, 0, wide(r"Local\NORY.Show.v1").as_ptr()) };
        if show.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let show = OwnedHandle(show);
        if existing {
            if unsafe { SetEvent(show.0) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            return Ok(None);
        }
        Ok(Some(Self {
            _mutex: mutex,
            show,
        }))
    }

    pub fn take_show_request(&self) -> bool {
        (unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(self.show.0, 0) })
            == windows_sys::Win32::Foundation::WAIT_OBJECT_0
    }
}

pub fn process_image(pid: u32) -> Option<String> {
    use windows_sys::Win32::System::Threading::*;
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let handle = OwnedHandle(handle);
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

pub fn running_processes() -> Vec<crate::applications::RunningProcess> {
    use crate::applications::RunningProcess;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    let mut valid = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    let mut seen = std::collections::HashSet::new();
    let mut processes = Vec::new();
    while valid {
        let length = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        if name.to_ascii_lowercase().ends_with(".exe") {
            let matcher = process_image(entry.th32ProcessID).unwrap_or_else(|| name.clone());
            if seen.insert(matcher.to_lowercase()) {
                processes.push(RunningProcess { name, matcher });
            }
        }
        valid = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    processes.sort_by_key(|process| process.name.to_lowercase());
    processes
}

pub fn installed_applications() -> Vec<crate::applications::InstalledApplication> {
    use crate::applications::InstalledApplication;
    use windows_sys::Win32::System::Registry::*;
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }
    fn value(key: HKEY, name: &str) -> Option<String> {
        let mut bytes = 0;
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        if unsafe {
            RegGetValueW(
                key,
                ptr::null(),
                wide(name).as_ptr(),
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut bytes,
            )
        } != 0
            || bytes > 65536
        {
            return None;
        }
        let mut buffer = vec![0u16; bytes as usize / 2 + 1];
        if unsafe {
            RegGetValueW(
                key,
                ptr::null(),
                wide(name).as_ptr(),
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        } != 0
        {
            return None;
        }
        let length = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }
    let mut applications = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            for (path, app_paths) in [
                (
                    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                    false,
                ),
                (r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths", true),
            ] {
                let mut key = ptr::null_mut();
                if unsafe { RegOpenKeyExW(root, wide(path).as_ptr(), 0, KEY_READ | view, &mut key) }
                    != 0
                {
                    continue;
                }
                let key = Key(key);
                for index in 0..10000 {
                    let mut name = [0u16; 256];
                    let mut length = name.len() as u32;
                    if unsafe {
                        RegEnumKeyExW(
                            key.0,
                            index,
                            name.as_mut_ptr(),
                            &mut length,
                            ptr::null(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                        )
                    } != 0
                    {
                        break;
                    }
                    let sub_name = String::from_utf16_lossy(&name[..length as usize]);
                    let mut child = ptr::null_mut();
                    if unsafe {
                        RegOpenKeyExW(key.0, name.as_ptr(), 0, KEY_READ | view, &mut child)
                    } != 0
                    {
                        continue;
                    }
                    let child = Key(child);
                    let Some(icon) = value(child.0, if app_paths { "" } else { "DisplayIcon" })
                    else {
                        continue;
                    };
                    let Some(matcher) = executable_icon_path(&icon) else {
                        continue;
                    };
                    if !Path::new(&matcher).is_file() || !seen.insert(matcher.to_lowercase()) {
                        continue;
                    }
                    let display = if app_paths {
                        Some(sub_name.trim_end_matches(".exe").to_string())
                    } else {
                        value(child.0, "DisplayName")
                    };
                    if let Some(name) = display {
                        applications.push(InstalledApplication { name, matcher });
                    }
                }
            }
        }
    }
    applications.sort_by_key(|application| application.name.to_lowercase());
    applications
}

fn executable_icon_path(value: &str) -> Option<String> {
    let value = value.trim();
    let path = if let Some(quoted) = value.strip_prefix('"') {
        quoted.split_once('"')?.0
    } else if let Some((path, index)) = value.rsplit_once(',') {
        if index.trim().parse::<i32>().is_ok() {
            path.trim()
        } else {
            value
        }
    } else {
        value
    };
    path.to_ascii_lowercase()
        .ends_with(".exe")
        .then(|| path.replace('/', "\\"))
}

pub(crate) fn direct_route() -> Result<crate::network::DirectRoute> {
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN, SOCKADDR_INET};
    let mut table = ptr::null_mut();
    let result = unsafe { GetIpForwardTable2(AF_INET, &mut table) };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result as i32).into());
    }
    struct Table(*mut MIB_IPFORWARD_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            unsafe {
                FreeMibTable(self.0.cast());
            }
        }
    }
    let table = Table(table);
    let rows = unsafe {
        std::slice::from_raw_parts((*table.0).Table.as_ptr(), (*table.0).NumEntries as usize)
    };
    let mut candidates = Vec::new();
    for route in rows
        .iter()
        .filter(|r| r.DestinationPrefix.PrefixLength == 0 && !r.Loopback)
    {
        let mut interface = MIB_IF_ROW2::default();
        interface.InterfaceLuid = route.InterfaceLuid;
        if unsafe { GetIfEntry2(&mut interface) } != 0
            || interface.OperStatus != IfOperStatusUp
            || !matches!(interface.Type, 6 | 71)
        {
            continue;
        }
        // Filter software adapters even if a driver reports Ethernet type.
        if interface.InterfaceAndOperStatusFlags._bitfield & 1 == 0 {
            continue;
        }
        let mut metric = MIB_IPINTERFACE_ROW::default();
        metric.Family = AF_INET;
        metric.InterfaceLuid = route.InterfaceLuid;
        let cost = if unsafe { GetIpInterfaceEntry(&mut metric) } == 0 {
            metric.Metric
        } else {
            0
        };
        candidates.push((route.Metric.saturating_add(cost), *route, interface));
    }
    candidates.sort_by_key(|(metric, _, _)| *metric);
    for (_, route, interface) in candidates {
        let mut destination = SOCKADDR_IN::default();
        destination.sin_family = AF_INET;
        destination.sin_addr.S_un.S_addr = u32::from_ne_bytes([1, 1, 1, 1]);
        let destination = SOCKADDR_INET { Ipv4: destination };
        let mut best = MIB_IPFORWARD_ROW2::default();
        let mut source = SOCKADDR_INET::default();
        if unsafe {
            GetBestRoute2(
                &route.InterfaceLuid,
                0,
                ptr::null(),
                &destination,
                0,
                &mut best,
                &mut source,
            )
        } != 0
        {
            continue;
        }
        let length = interface
            .Alias
            .iter()
            .position(|v| *v == 0)
            .unwrap_or(interface.Alias.len());
        return Ok(crate::network::DirectRoute {
            interface: String::from_utf16_lossy(&interface.Alias[..length]),
            source: std::net::Ipv4Addr::from(
                unsafe { source.Ipv4.sin_addr.S_un.S_addr }.to_ne_bytes(),
            ),
            index: route.InterfaceIndex,
        });
    }
    bail!("Не найден внешний IPv4-интерфейс: проверка через TUN не выполняется")
}

pub fn test_latency(address: &str, timeout: std::time::Duration) -> Result<u32> {
    let route = crate::network::DirectRoute::discover()?;
    let address = route.resolve(address)?;
    test_latency_direct(address, timeout, route.source)
}

pub(crate) fn test_latency_direct(
    address: std::net::Ipv4Addr,
    timeout: std::time::Duration,
    source_ip: std::net::Ipv4Addr,
) -> Result<u32> {
    use std::net::{IpAddr, SocketAddr};
    use windows_sys::Win32::Networking::WinSock::{AF_INET6, SOCKADDR_IN6};
    let addresses = vec![SocketAddr::new(IpAddr::V4(address), 0)];
    let address = addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.first())
        .context("Не удалось определить IP сервера")?;
    struct Icmp(HANDLE);
    impl Drop for Icmp {
        fn drop(&mut self) {
            unsafe { IcmpCloseHandle(self.0) };
        }
    }
    let handle = unsafe {
        if address.is_ipv4() {
            IcmpCreateFile()
        } else {
            Icmp6CreateFile()
        }
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let handle = Icmp(handle);
    let timeout_ms = timeout.as_millis().clamp(100, 60000) as u32;
    let data = b"NORY ICMP latency probe";
    let mut samples = Vec::with_capacity(5);
    for request in 0..5 {
        // u64 allocation ensures alignment for either Windows reply structure.
        let mut reply = [0u64; 128];
        let (count, status, round_trip) = match address.ip() {
            IpAddr::V4(ip) => {
                let count = unsafe {
                    IcmpSendEcho2Ex(
                        handle.0,
                        ptr::null_mut(),
                        None,
                        ptr::null(),
                        u32::from_ne_bytes(source_ip.octets()),
                        u32::from_ne_bytes(ip.octets()),
                        data.as_ptr().cast(),
                        data.len() as u16,
                        ptr::null(),
                        reply.as_mut_ptr().cast(),
                        std::mem::size_of_val(&reply) as u32,
                        timeout_ms,
                    )
                };
                let result = unsafe { &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>() };
                (count, result.Status, result.RoundTripTime)
            }
            IpAddr::V6(ip) => {
                let mut source = SOCKADDR_IN6::default();
                source.sin6_family = AF_INET6;
                let mut destination = source;
                destination.sin6_addr.u.Byte = ip.octets();
                if let SocketAddr::V6(address) = address {
                    destination.Anonymous.sin6_scope_id = address.scope_id();
                }
                let count = unsafe {
                    Icmp6SendEcho2(
                        handle.0,
                        ptr::null_mut(),
                        None,
                        ptr::null(),
                        &source,
                        &destination,
                        data.as_ptr().cast(),
                        data.len() as u16,
                        ptr::null(),
                        reply.as_mut_ptr().cast(),
                        std::mem::size_of_val(&reply) as u32,
                        timeout_ms,
                    )
                };
                let result = unsafe { &*reply.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>() };
                (count, result.Status, result.RoundTripTime)
            }
        };
        if count > 0 && status == IP_SUCCESS {
            samples.push(round_trip);
        }
        if request < 4 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    if samples.is_empty() {
        bail!("Сервер не отвечает на ICMP");
    }
    Ok((samples
        .iter()
        .map(|v| *v as u64)
        .sum::<u64>()
        .div_ceil(samples.len() as u64)) as u32)
}

/// Ask Windows to elevate only our installed helper, never the WebView GUI.
/// The service validates the helper's actual token and binds consent to the
/// live GUI process, rather than trusting a flag or a user-writable file.
pub(crate) fn request_xray_access() -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    };
    let directory = std::env::current_exe()?
        .parent()
        .context("Не найден каталог установки NORY")?
        .to_path_buf();
    let helper = directory.join("nory-helper.exe");
    if !helper.is_file() {
        bail!("Не найден nory-helper.exe. Переустановите NORY через установщик");
    }
    let verb = wide("runas");
    let executable = wide(&helper);
    let directory = wide(&directory);
    let arguments = wide(format!("authorize-xray {}", std::process::id()));
    let mut execute = SHELLEXECUTEINFOW::default();
    execute.cbSize = std::mem::size_of_val(&execute) as u32;
    execute.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    execute.lpVerb = verb.as_ptr();
    execute.lpFile = executable.as_ptr();
    execute.lpParameters = arguments.as_ptr();
    execute.lpDirectory = directory.as_ptr();
    execute.nShow = SW_HIDE;
    if unsafe { ShellExecuteExW(&mut execute) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            bail!(
                "Подключение отменено: Xray TUN требует прав администратора. Подтвердите запрос Windows и попробуйте ещё раз"
            );
        }
        return Err(error).context("Не удалось запросить права администратора для Xray TUN");
    }
    let process = OwnedHandle(execute.hProcess);
    if process.0.is_null() {
        bail!("Windows не вернула процесс авторизации Xray TUN");
    }
    if unsafe { WaitForSingleObject(process.0, 35_000) } != WAIT_OBJECT_0 {
        bail!("Не удалось дождаться авторизации Xray TUN. Попробуйте ещё раз");
    }
    let mut code = 1;
    if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 || code != 0 {
        bail!("Не удалось разрешить Xray TUN. Проверьте службу NoryTunnel или переустановите NORY");
    }
    Ok(())
}

pub fn launch_installer(path: &Path) -> Result<()> {
    use windows_sys::Win32::UI::{
        Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };
    let verb = wide("runas");
    let path = wide(path);
    let arguments = wide(format!("/S /UPDATE /WAITPID={}", std::process::id()));
    let mut execute = SHELLEXECUTEINFOW::default();
    execute.cbSize = std::mem::size_of_val(&execute) as u32;
    execute.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    execute.lpVerb = verb.as_ptr();
    execute.lpFile = path.as_ptr();
    execute.lpParameters = arguments.as_ptr();
    execute.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut execute) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(1223) {
            bail!("Обновление отменено в окне Windows. NORY продолжает работать");
        }
        return Err(error).context("Не удалось запустить установщик NORY");
    }
    let _process = OwnedHandle(execute.hProcess);
    Ok(())
}
