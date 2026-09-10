use std::process::Command;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// sing-box creates the adapter before finishing routes and other services.
/// Its root startup message is emitted only after Box.Start succeeds.
pub(crate) struct SingBoxStartup(Arc<AtomicBool>);

impl SingBoxStartup {
    pub(crate) fn capture(
        reader: impl Read + Send + 'static,
        mut log: impl Write + Send + 'static,
    ) -> io::Result<Self> {
        let started = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&started);
        std::thread::Builder::new()
            .name("sing-box-log".into())
            .spawn(move || {
                for line in BufReader::new(reader).split(b'\n') {
                    let Ok(line) = line else { break };
                    let message = String::from_utf8_lossy(&line);
                    if message.contains(" sing-box started (") && message.trim_end().ends_with("s)")
                    {
                        signal.store(true, Ordering::Release);
                    } else if signal.load(Ordering::Acquire)
                        && (message.contains(" INFO ") || message.starts_with("INFO["))
                    {
                        // Info is needed for readiness, but logging every
                        // connection/DNS answer keeps disks and journals busy.
                        continue;
                    }
                    // Keep draining after readiness, even if the log sink fails.
                    let _ = log.write_all(&line).and_then(|_| log.write_all(b"\n"));
                }
            })?;
        Ok(Self(started))
    }

    pub(crate) fn ready(&self, child: &mut Child, interface_present: bool) -> io::Result<bool> {
        if let Some(exit) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "sing-box завершился при запуске ({exit})"
            )));
        }
        Ok(interface_present && self.0.load(Ordering::Acquire))
    }
}

/// Keep command setup uniform at call sites.
pub fn hide_window(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(all(test, target_os = "linux"))]
mod tun_startup_tests {
    use super::*;
    use std::{
        os::unix::net::UnixStream,
        process::Stdio,
        time::{Duration, Instant},
    };

    struct TestChild(Child);

    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_ready(startup: &SingBoxStartup, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !startup.ready(child, true).unwrap() {
            assert!(
                Instant::now() < deadline,
                "startup message was not detected"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn adapter_presence_waits_for_complete_startup_and_live_process() {
        let mut child = TestChild(Command::new("/usr/bin/sleep").arg("30").spawn().unwrap());
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let startup = SingBoxStartup::capture(reader, io::sink()).unwrap();
        writer
            .write_all(b"INFO[0000] inbound/tun[tun-in]: started at nory0\n")
            .unwrap();
        assert!(!startup.ready(&mut child.0, true).unwrap());
        writer
            .write_all(b"INFO[0000] sing-box pre-started (0.01s)\n")
            .unwrap();
        assert!(!startup.ready(&mut child.0, true).unwrap());
        writer.write_all(b"INFO[0000] sing-box started (").unwrap();
        assert!(!startup.ready(&mut child.0, true).unwrap());
        writer.write_all(b"0.02s)\r\n").unwrap();
        wait_ready(&startup, &mut child.0);
        assert!(!startup.ready(&mut child.0, false).unwrap());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(startup.ready(&mut child.0, true).is_err());
    }

    #[test]
    fn startup_failure_after_adapter_creation_is_not_ready() {
        let mut child = TestChild(Command::new("/bin/sh")
            .args(["-c", "echo 'INFO[0000] inbound/tun[tun-in]: started at nory0' >&2; echo 'FATAL configure routes' >&2; exit 1"])
            .stderr(Stdio::piped()).spawn().unwrap());
        let startup = SingBoxStartup::capture(child.0.stderr.take().unwrap(), io::sink()).unwrap();
        child.0.wait().unwrap();
        assert!(startup.ready(&mut child.0, true).is_err());
    }

    #[test]
    #[ignore = "requires NORY_TEST_SING_BOX; starts the core without TUN or remote connections"]
    fn bundled_sing_box_reports_full_startup() {
        let binary = std::env::var_os("NORY_TEST_SING_BOX").expect("NORY_TEST_SING_BOX");
        let config = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(config.path(), br#"{"log":{"level":"info","timestamp":true},"outbounds":[{"type":"direct","tag":"direct"}]}"#).unwrap();
        let mut child = TestChild(
            Command::new(binary)
                .args(["run", "--disable-color", "-c"])
                .arg(config.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let startup = SingBoxStartup::capture(child.0.stderr.take().unwrap(), io::sink()).unwrap();
        wait_ready(&startup, &mut child.0);
    }
}
