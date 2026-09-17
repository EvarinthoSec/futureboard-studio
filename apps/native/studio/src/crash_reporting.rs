//! Crashpad installation for the native Studio process.
//!
//! The handler is downloaded by `crashpad-rs-sys`'s `prebuilt` feature and
//! copied beside the application binary by the build dependency. Runtime
//! paths deliberately live under app data rather than the install directory,
//! because packaged applications may be read-only and crash reports can be
//! written before the UI starts.

use crashpad_rs::{CrashpadClient, CrashpadConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

const CRASHPAD_PATH: &str = "/crashpad/minidump";

static CLIENT: OnceLock<CrashpadClient> = OnceLock::new();

/// Start Crashpad for the main browser process.
///
/// Crash reporting is best-effort: a missing endpoint or handler must never
/// prevent Studio from opening. Crashpad persists pending reports locally and
/// uploads them asynchronously, so startup does not wait for the API.
pub fn install() {
    if CLIENT.get().is_some() {
        return;
    }

    let Some(upload_url) = upload_url() else {
        eprintln!("[crashpad] disabled: account API endpoint is not configured");
        return;
    };

    let paths = sphere_ui_components::paths::FutureboardPaths::resolve();
    let crashpad_dir = paths.app_data.join("Crashpad");
    let Some(handler_path) = handler_path() else {
        eprintln!("[crashpad] disabled: target uses an in-process handler");
        return;
    };
    let config = CrashpadConfig::builder()
        .handler_path(handler_path)
        .database_path(crashpad_dir.join("database"))
        .metrics_path(crashpad_dir.join("metrics"))
        .url(upload_url)
        .rate_limit(true)
        .upload_gzip(true)
        .periodic_tasks(true)
        .identify_client_via_url(true)
        .build();

    let mut annotations = HashMap::new();
    annotations.insert("format".to_string(), "minidump".to_string());
    annotations.insert("prod".to_string(), "futureboard-studio".to_string());
    annotations.insert("ver".to_string(), env!("CARGO_PKG_VERSION").to_string());
    annotations.insert("platform".to_string(), std::env::consts::OS.to_string());
    annotations.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    annotations.insert("edition".to_string(), edition_name().to_string());

    let client = match CrashpadClient::new() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[crashpad] client initialization failed: {error}");
            return;
        }
    };
    if let Err(error) = client.start_with_config(&config, &annotations) {
        eprintln!("[crashpad] handler startup failed: {error}");
        return;
    }

    let _ = CLIENT.set(client);
    eprintln!("[crashpad] handler started; upload endpoint configured");
}

fn upload_url() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("FUTUREBOARD_CRASHPAD_URL") {
        let value = value.trim().trim_end_matches('/');
        if acceptable_endpoint(value) {
            return Some(value.to_string());
        }
        eprintln!("[crashpad] ignoring invalid FUTUREBOARD_CRASHPAD_URL");
    }

    sphere_ui_components::auth::api_base_url().map(|base| format!("{base}{CRASHPAD_PATH}"))
}

fn acceptable_endpoint(url: &str) -> bool {
    url.starts_with("https://")
        || (cfg!(debug_assertions)
            && (url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")))
}

fn handler_path() -> Option<PathBuf> {
    if cfg!(any(
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos"
    )) {
        return None;
    }

    if let Some(path) = std::env::var_os("CRASHPAD_HANDLER").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }

    let name = if cfg!(target_os = "android") {
        "libcrashpad_handler.so"
    } else if cfg!(windows) {
        "crashpad_handler.exe"
    } else {
        "crashpad_handler"
    };
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            let beside_executable = parent.join(name);
            if beside_executable.is_file() {
                return Some(beside_executable);
            }
        }
    }

    Some(
        option_env!("CRASHPAD_HANDLER_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(name)),
    )
}

fn edition_name() -> &'static str {
    if cfg!(feature = "professional") {
        "professional"
    } else {
        "community"
    }
}
