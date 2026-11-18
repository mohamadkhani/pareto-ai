//! Pareto AI desktop shell.
//!
//! A thin Tauri window around the unchanged gateway binary (decision D43):
//!
//! 1. run the gateway **sidecar** on the fixed loopback port `127.0.0.1:8081`
//!    (fixed, not random, so API clients can rely on it) with a per-app data
//!    directory (`<app-data>/gateway`), insecure cookies (plain HTTP on
//!    loopback) and no admin environment, so the first run shows the setup
//!    wizard,
//! 2. poll `GET /health` for up to 20 s,
//! 3. open the main window at `http://127.0.0.1:8081/` (the gateway keeps
//!    serving its embedded console; no assets are duplicated here),
//! 4. keep a system-tray icon: closing the window hides it instead of
//!    stopping the gateway, so the service keeps running in the background,
//! 5. kill the sidecar when the app exits (tray "Quit Pareto AI").
//!
//! The sidecar's stdout/stderr go to `<app-data>/gateway/sidecar.log`; the
//! tail of that file is shown in a native error dialog if startup fails.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// How long the gateway may take to serve `/health` before startup fails.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(20);
/// Gap between `/health` attempts.
const HEALTH_INTERVAL: Duration = Duration::from_millis(200);
/// Connect timeout for one `/health` attempt.
const HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// How many log lines the failure dialog shows.
const LOG_TAIL_LINES: usize = 40;
/// Name of the process log inside the gateway data directory.
const LOG_FILE: &str = "sidecar.log";
/// Loopback port the gateway listens on. Fixed (never random) so API clients
/// and the operator always know where the service is (D46).
const GATEWAY_PORT: u16 = 8081;
/// Label of the main (and only) window.
const MAIN_WINDOW: &str = "main";
/// Tray menu item id: show the main window.
const TRAY_OPEN: &str = "open";
/// Tray menu item id: stop the gateway and exit the app.
const TRAY_QUIT: &str = "quit";

/// The running gateway process, killed on exit.
struct Gateway {
    child: Mutex<Option<Child>>,
}

impl Gateway {
    /// Kills and reaps the sidecar; safe to call more than once.
    fn stop(&self) {
        let child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Why the gateway could not be started, plus what to show the operator.
struct StartupError {
    message: String,
    log_tail: String,
}

fn main() {
    let app = tauri::Builder::default()
        // Must be the first plugin: a second launch focuses the first window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            if let Err(error) = start(app.handle()) {
                show_startup_error(app.handle(), &error);
                app.handle().exit(1);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            // No window exists yet, so this is the process log's job.
            eprintln!("pareto-desktop: cannot start the Tauri runtime: {error}");
            std::process::exit(1);
        });

    app.run(|app_handle, event| match event {
        // Closing the main window only hides it: the gateway keeps serving in
        // the background and the tray icon stays alive (full quit is a tray
        // menu item). Any other window keeps the default close behavior.
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW => {
            if let Some(window) = app_handle.get_webview_window(MAIN_WINDOW) {
                let _ = window.hide();
            }
            api.prevent_close();
        }
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            if let Some(gateway) = app_handle.try_state::<Gateway>() {
                gateway.stop();
            }
        }
        _ => {}
    });
}

/// Starts the sidecar, waits for `/health`, and opens the main window.
fn start(app: &tauri::AppHandle) -> Result<(), StartupError> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| StartupError {
            message: format!("cannot resolve the app data directory: {error}"),
            log_tail: String::new(),
        })?
        .join("gateway");
    fs::create_dir_all(&data_dir).map_err(|error| StartupError {
        message: format!("cannot create {}: {error}", data_dir.display()),
        log_tail: String::new(),
    })?;

    let port = GATEWAY_PORT;
    let binary = sidecar_path().ok_or_else(|| StartupError {
        message: format!(
            "the gateway sidecar is missing.\n\nLooked for `pareto-gateway` next to \
             {} and in the crate's binaries/ directory. Reinstall the app, or set \
             PARETO_GATEWAY_BIN.",
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "the app binary".into())
        ),
        log_tail: String::new(),
    })?;

    let log_path = data_dir.join(LOG_FILE);
    let log = File::create(&log_path).map_err(|error| StartupError {
        message: format!(
            "cannot write the gateway log {}: {error}",
            log_path.display()
        ),
        log_tail: String::new(),
    })?;
    let stderr = log.try_clone().map_err(|error| StartupError {
        message: format!(
            "cannot open the gateway log {}: {error}",
            log_path.display()
        ),
        log_tail: String::new(),
    })?;

    let mut command = Command::new(&binary);
    command
        .env("GATEWAY_DATA_DIR", &data_dir)
        .env("GATEWAY_LISTEN", format!("127.0.0.1:{port}"))
        // Loopback over plain HTTP: a Secure cookie would never be sent back.
        .env("GATEWAY_INSECURE_COOKIES", "1")
        // The wizard is the intended first run; inherited admin credentials
        // would silently skip it.
        .env_remove("GATEWAY_ADMIN_EMAIL")
        .env_remove("GATEWAY_ADMIN_PASSWORD")
        .env_remove("GATEWAY_MASTER_KEY")
        .current_dir(&data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes next to the window.
        command.creation_flags(0x0800_0000);
    }

    let child = command.spawn().map_err(|error| StartupError {
        message: format!(
            "cannot start the gateway sidecar {}: {error}",
            binary.display()
        ),
        log_tail: log_tail(&log_path),
    })?;
    app.manage(Gateway {
        child: Mutex::new(Some(child)),
    });

    if let Err(detail) = wait_for_health(port, HEALTH_TIMEOUT) {
        return Err(StartupError {
            message: format!(
                "the gateway did not become healthy on 127.0.0.1:{port} within {} s.\n\n{detail}",
                HEALTH_TIMEOUT.as_secs()
            ),
            log_tail: log_tail(&log_path),
        });
    }

    let url = format!("http://127.0.0.1:{port}/")
        .parse()
        .map_err(|error| StartupError {
            message: format!("cannot build the gateway URL: {error}"),
            log_tail: log_tail(&log_path),
        })?;

    WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(url))
        .title("Pareto AI")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 600.0)
        .build()
        .map_err(|error| StartupError {
            message: format!("cannot open the main window: {error}"),
            log_tail: log_tail(&log_path),
        })?;

    build_tray(app).map_err(|error| StartupError {
        message: format!("cannot create the tray icon: {error}"),
        log_tail: log_tail(&log_path),
    })?;

    Ok(())
}

/// Shows the main window, restoring it from the tray or a minimized state.
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Stops the gateway and exits the app; the tray menu's "Quit".
fn quit_app(app: &tauri::AppHandle) {
    if let Some(gateway) = app.try_state::<Gateway>() {
        gateway.stop();
    }
    app.exit(0);
}

/// Builds the tray icon and its menu ("Open Pareto AI", "Quit Pareto AI").
///
/// The icon is registered with the app's resource table, so it stays alive
/// after this function returns. Failures: no tray-capable desktop or an
/// unwritable icon temp dir on Linux.
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, TRAY_OPEN, "Open Pareto AI", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, TRAY_QUIT, "Quit Pareto AI", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(MAIN_WINDOW)
        .tooltip("Pareto AI — gateway running")
        .menu(&menu)
        // macOS convention: the menu opens on left click. Elsewhere the right
        // click shows the menu and a left double-click opens the window.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id.as_ref() {
            TRAY_OPEN => show_main_window(app),
            TRAY_QUIT => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Shows the native error dialog (blocks until dismissed).
fn show_startup_error(app: &tauri::AppHandle, error: &StartupError) {
    let mut body = error.message.clone();
    if !error.log_tail.is_empty() {
        body.push_str("\n\nGateway log (last lines):\n");
        body.push_str(&error.log_tail);
    }
    app.dialog()
        .message(body)
        .kind(MessageDialogKind::Error)
        .title("Pareto AI could not start")
        .blocking_show();
}

/// Resolves the sidecar binary.
///
/// Tauri bundles `externalBin` next to the app binary on every platform we
/// ship (and in `usr/lib/<product>/` for some Linux layouts), so the app's own
/// directory is tried first. `CARGO_MANIFEST_DIR/binaries/` is the dev path
/// written by `build.rs`.
fn sidecar_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PARETO_GATEWAY_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    let name = format!("pareto-gateway{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(&name));
            // deb/rpm layouts that keep payloads outside /usr/bin.
            candidates.push(dir.join("../lib/pareto-desktop").join(&name));
            candidates.push(dir.join("../lib/Pareto AI").join(&name));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!(
                "pareto-gateway-{}{}",
                env!("PARETO_TARGET_TRIPLE"),
                std::env::consts::EXE_SUFFIX
            )),
    );
    candidates.into_iter().find(|path| path.is_file())
}

/// Polls `/health` until it answers 200 or the deadline passes.
///
/// Failure message: the last observed error, or a timeout notice.
fn wait_for_health(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("no response yet");
    while Instant::now() < deadline {
        match health_once(port) {
            Ok(true) => return Ok(()),
            Ok(false) => last = format!("GET /health did not answer 200 ({last})"),
            Err(error) => last = format!("GET /health failed: {error}"),
        }
        std::thread::sleep(HEALTH_INTERVAL);
    }
    Err(last)
}

/// One blocking `GET /health`; true means a 200 status line.
fn health_once(port: u16) -> Result<bool, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, HEALTH_CONNECT_TIMEOUT)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;

    let mut response = [0_u8; 128];
    let read = stream
        .read(&mut response)
        .map_err(|error| error.to_string())?;
    let head = String::from_utf8_lossy(&response[..read]);
    Ok(head
        .lines()
        .next()
        .map(|status| status.contains(" 200"))
        .unwrap_or(false))
}

/// Last [`LOG_TAIL_LINES`] lines of a log file; a short notice when it is
/// missing or empty.
fn log_tail(path: &Path) -> String {
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().rev().take(LOG_TAIL_LINES).collect();
    if lines.is_empty() {
        return "the gateway wrote no log output".to_string();
    }
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Binds port 0 on loopback and returns the port the OS assigned; a test
    /// helper for probe servers that need any free port. The gateway itself
    /// uses the fixed [`GATEWAY_PORT`].
    fn free_loopback_port() -> std::io::Result<u16> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(port)
    }

    #[test]
    fn free_port_is_usable_loopback() {
        let port = free_loopback_port().expect("bind port 0");
        assert!(port >= 1024);
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("port is free again");
        drop(listener);
    }

    #[test]
    fn health_probe_reports_a_plain_http_server() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 256];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        assert!(health_once(port).expect("probe"));

        // A non-200 answer is not healthy.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 256];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\n\r\n");
            }
        });
        assert!(!health_once(port).expect("probe"));
    }

    #[test]
    fn health_wait_times_out_against_a_closed_port() {
        // A port that was just released refuses connections on every platform.
        let port = free_loopback_port().expect("port");
        let started = Instant::now();
        let error = wait_for_health(port, Duration::from_millis(300)).expect_err("must time out");
        assert!(!error.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn log_tail_keeps_the_last_lines_in_order() {
        let dir = std::env::temp_dir().join("pareto-desktop-log-tail");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("sidecar.log");
        fs::write(&path, "one\ntwo\nthree\n").expect("write");
        assert_eq!(log_tail(&path), "one\ntwo\nthree");

        fs::write(&path, "").expect("write");
        assert_eq!(log_tail(&path), "the gateway wrote no log output");
        let _ = fs::remove_dir_all(&dir);
    }
}
