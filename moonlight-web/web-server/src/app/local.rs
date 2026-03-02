use std::time::Duration;
use std::{env, sync::LazyLock};

use log::{info, warn};
use moonlight_common::PairPin;
use tokio::time::{sleep, timeout};

use crate::app::{
    App, AppError,
    host::{App as HostApp, Host},
};

const LOCAL_SUNSHINE_ADDRESS: &str = "127.0.0.1";
const DEFAULT_SUNSHINE_API_PORT: u16 = 47990;
const DEFAULT_SUNSHINE_API_USERNAME: &str = "yuu";
const DEFAULT_SUNSHINE_API_PASSWORD: &str = "yuu";
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
const PIN_RETRY_INITIAL: Duration = Duration::from_millis(100);
const PIN_RETRY_MAX: Duration = Duration::from_secs(2);

static SUNSHINE_API_PORT: LazyLock<u16> = LazyLock::new(|| {
    env::var("SUNSHINE_API_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_SUNSHINE_API_PORT)
});

static SUNSHINE_API_USERNAME: LazyLock<String> = LazyLock::new(|| {
    env::var("SUNSHINE_API_USERNAME").unwrap_or_else(|_| DEFAULT_SUNSHINE_API_USERNAME.to_string())
});

static SUNSHINE_API_PASSWORD: LazyLock<String> = LazyLock::new(|| {
    env::var("SUNSHINE_API_PASSWORD").unwrap_or_else(|_| DEFAULT_SUNSHINE_API_PASSWORD.to_string())
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFailureCode {
    SunshineNotReady,
    AssociationFailed,
    PairingFailed,
    DesktopAppNotFound,
    StreamBackendUnhealthy,
}

#[derive(Debug, Clone)]
pub struct LocalEnsureError {
    pub code: LocalFailureCode,
    pub status_text: String,
    pub detail: Option<String>,
    pub retryable: bool,
}

impl LocalEnsureError {
    fn new(
        code: LocalFailureCode,
        status_text: impl Into<String>,
        detail: Option<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            status_text: status_text.into(),
            detail,
            retryable,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalBootstrap {
    pub host_id: u32,
    pub app_id: u32,
    pub status_text: String,
    pub can_take_over: bool,
    pub observe_mode_default: bool,
}

#[derive(Debug, Clone)]
pub struct LocalStatus {
    pub ready: bool,
    pub host_id: Option<u32>,
    pub app_id: Option<u32>,
    pub status_text: String,
    pub can_take_over: bool,
    pub observe_mode_default: bool,
    pub failure: Option<LocalFailureCode>,
}

pub async fn get_local_status(app: &App) -> LocalStatus {
    match local_status_inner(app).await {
        Ok(status) => status,
        Err(err) => LocalStatus {
            ready: false,
            host_id: None,
            app_id: None,
            status_text: err.status_text,
            can_take_over: true,
            observe_mode_default: true,
            failure: Some(err.code),
        },
    }
}

pub async fn ensure_local_bootstrap(app: &App) -> Result<LocalBootstrap, LocalEnsureError> {
    let mut host = ensure_local_host(app, true).await?;
    let host_id = host.id().0;

    let apps = match host.list_apps().await {
        Ok(apps) => apps,
        Err(AppError::HostNotPaired) => {
            info!("local stream host is not paired; starting internal pairing flow");
            auto_pair_host(app, &mut host).await?;
            host.list_apps().await.map_err(map_backend_error)?
        }
        Err(err) => {
            return Err(map_backend_error(err));
        }
    };

    let app_id = pick_desktop_app(&apps)
        .ok_or_else(|| {
            LocalEnsureError::new(
                LocalFailureCode::DesktopAppNotFound,
                "No launchable desktop app is available yet.",
                None,
                true,
            )
        })?
        .id
        .0;

    info!(
        "local stream bootstrap served host_id={} app_id={}",
        host_id, app_id
    );

    Ok(LocalBootstrap {
        host_id,
        app_id,
        status_text: "Computer stream is ready.".to_string(),
        can_take_over: true,
        observe_mode_default: true,
    })
}

async fn local_status_inner(app: &App) -> Result<LocalStatus, LocalEnsureError> {
    let mut host = ensure_local_host(app, false).await?;
    let host_id = host.id().0;

    let apps = host.list_apps().await.map_err(map_backend_error)?;
    let app_id = pick_desktop_app(&apps).map(|app| app.id.0);

    match app_id {
        Some(app_id) => Ok(LocalStatus {
            ready: true,
            host_id: Some(host_id),
            app_id: Some(app_id),
            status_text: "Computer stream is ready.".to_string(),
            can_take_over: true,
            observe_mode_default: true,
            failure: None,
        }),
        None => Err(LocalEnsureError::new(
            LocalFailureCode::DesktopAppNotFound,
            "No launchable desktop app is available yet.",
            None,
            true,
        )),
    }
}

async fn ensure_local_host(app: &App, allow_create: bool) -> Result<Host, LocalEnsureError> {
    let default_http_port = app.config().moonlight.default_http_port;
    let hosts = app.list_hosts().await.map_err(|error| {
        LocalEnsureError::new(
            LocalFailureCode::AssociationFailed,
            "Failed to inspect local stream associations.",
            Some(error.to_string()),
            true,
        )
    })?;

    for host in hosts {
        let (address, port) = match host.address_port().await {
            Ok(value) => value,
            Err(error) => {
                warn!("failed to read host address for local stream association: {error}");
                continue;
            }
        };
        if address == LOCAL_SUNSHINE_ADDRESS && port == default_http_port {
            return Ok(host);
        }
    }

    if !allow_create {
        return Err(LocalEnsureError::new(
            LocalFailureCode::AssociationFailed,
            "No local Sunshine association exists yet.",
            None,
            true,
        ));
    }

    info!(
        "local stream association begin address={} port={}",
        LOCAL_SUNSHINE_ADDRESS, default_http_port
    );
    let host = app
        .add_host(LOCAL_SUNSHINE_ADDRESS.to_string(), default_http_port)
        .await
        .map_err(|error| match error {
            AppError::HostNotFound | AppError::HostOffline => LocalEnsureError::new(
                LocalFailureCode::SunshineNotReady,
                "Sunshine is not ready yet.",
                Some(error.to_string()),
                true,
            ),
            _ => LocalEnsureError::new(
                LocalFailureCode::AssociationFailed,
                "Failed to create local Sunshine association.",
                Some(error.to_string()),
                true,
            ),
        })?;

    info!("local stream association success host_id={}", host.id().0);
    Ok(host)
}

async fn auto_pair_host(app: &App, host: &mut Host) -> Result<(), LocalEnsureError> {
    info!("local stream pairing begin");
    let pin = PairPin::generate().map_err(|error| {
        LocalEnsureError::new(
            LocalFailureCode::PairingFailed,
            "Failed to start pairing.",
            Some(error.to_string()),
            false,
        )
    })?;
    let pin_text = pin.to_string();
    let pair_name = app.config().moonlight.pair_device_name.clone();
    let sunshine_port = app.config().moonlight.default_http_port;

    let pin_task = tokio::spawn(async move {
        submit_sunshine_pin_loop(pin_text, pair_name, sunshine_port).await;
    });

    let pair_result = timeout(PAIR_TIMEOUT, host.pair(pin)).await;
    pin_task.abort();
    let _ = pin_task.await;

    match pair_result {
        Ok(Ok(())) | Ok(Err(AppError::HostPaired)) => {
            info!("local stream pairing success");
            Ok(())
        }
        Ok(Err(error)) => {
            warn!("local stream pairing failed: {error}");
            Err(LocalEnsureError::new(
                LocalFailureCode::PairingFailed,
                "Automatic pairing failed.",
                Some(error.to_string()),
                true,
            ))
        }
        Err(_) => {
            warn!("local stream pairing timed out");
            Err(LocalEnsureError::new(
                LocalFailureCode::PairingFailed,
                "Automatic pairing timed out.",
                None,
                true,
            ))
        }
    }
}

async fn submit_sunshine_pin_loop(pin: String, name: String, port: u16) {
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!("failed to build sunshine pin client: {error}");
            return;
        }
    };
    let pin_url = format!("https://{LOCAL_SUNSHINE_ADDRESS}:{}/api/pin", *SUNSHINE_API_PORT);

    let mut delay = PIN_RETRY_INITIAL;
    let mut accepted_logged = false;
    loop {
        let response = client
            .post(&pin_url)
            .basic_auth(
                SUNSHINE_API_USERNAME.as_str(),
                Some(SUNSHINE_API_PASSWORD.as_str()),
            )
            .json(&serde_json::json!({
                "pin": &pin,
                "name": &name,
            }))
            .send()
            .await;

        match response {
            Ok(value) if value.status().is_success() => {
                let status = value.status();
                let body = value.text().await.unwrap_or_default();
                if sunshine_pin_accepted(&body) {
                    if !accepted_logged {
                        info!(
                            "sunshine /api/pin accepted pairing pin (api_port={} host_port={})",
                            *SUNSHINE_API_PORT,
                            port
                        );
                    }
                    // Stop posting once Sunshine accepts the PIN. Re-submitting can reset the
                    // pairing state and race against the in-flight Moonlight pair handshake.
                    return;
                }
                warn!(
                    "sunshine /api/pin returned {} but pairing not yet accepted body={} (api_port={} host_port={})",
                    status,
                    body,
                    *SUNSHINE_API_PORT,
                    port
                );
            }
            Ok(value) => {
                warn!(
                    "sunshine /api/pin returned {} (api_port={} host_port={})",
                    value.status(),
                    *SUNSHINE_API_PORT,
                    port
                );
            }
            Err(error) => {
                warn!(
                    "sunshine /api/pin request failed (api_port={} host_port={}): {error}",
                    *SUNSHINE_API_PORT,
                    port
                );
            }
        }

        sleep(delay).await;
        delay = delay.saturating_mul(2).min(PIN_RETRY_MAX);
    }
}

fn sunshine_pin_accepted(body: &str) -> bool {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    match json.get("status") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn map_backend_error(error: AppError) -> LocalEnsureError {
    match error {
        AppError::HostNotFound | AppError::HostOffline => LocalEnsureError::new(
            LocalFailureCode::SunshineNotReady,
            "Sunshine is not ready yet.",
            Some(error.to_string()),
            true,
        ),
        AppError::HostNotPaired => LocalEnsureError::new(
            LocalFailureCode::PairingFailed,
            "Sunshine host is not paired yet.",
            Some(error.to_string()),
            true,
        ),
        AppError::HostPaired => LocalEnsureError::new(
            LocalFailureCode::PairingFailed,
            "Sunshine host pairing is in an invalid state.",
            Some(error.to_string()),
            true,
        ),
        AppError::MoonlightApi(_) | AppError::Forbidden => LocalEnsureError::new(
            LocalFailureCode::StreamBackendUnhealthy,
            "Stream backend is unhealthy.",
            Some(error.to_string()),
            true,
        ),
        _ => LocalEnsureError::new(
            LocalFailureCode::AssociationFailed,
            "Local stream association failed.",
            Some(error.to_string()),
            true,
        ),
    }
}

fn pick_desktop_app(apps: &[HostApp]) -> Option<&HostApp> {
    apps.iter()
        .find(|app| app.title.to_ascii_lowercase().contains("desktop"))
        .or_else(|| apps.first())
}
