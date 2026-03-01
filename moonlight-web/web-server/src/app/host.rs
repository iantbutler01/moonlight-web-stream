use std::{
    fmt::{Debug, Formatter},
    str::FromStr,
};

use actix_web::web::Bytes;
use common::api_bindings::{self, DetailedHost, HostState, PairStatus, UndetailedHost};
use log::warn;
use moonlight_common::{
    PairPin, ServerState,
    high::broadcast_magic_packet,
    network::{
        self, ApiError, ClientAppBoxArtRequest, ClientInfo, HostInfo, host_app_box_art,
        host_app_list, host_cancel, host_info,
        request_client::{RequestClient, RequestError},
    },
    pair::{PairSuccess, generate_new_client, host_pair},
};
use uuid::Uuid;

use crate::app::{
    AppError, AppInner, AppRef, MoonlightClient,
    storage::{StorageHost, StorageHostModify, StorageHostPairInfo},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostId(pub u32);

pub struct Host {
    pub(super) app: AppRef,
    pub(super) id: HostId,
    pub(super) cache_storage: Option<StorageHost>,
    pub(super) cache_host_info: Option<HostInfo>,
}

impl Debug for Host {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppId(pub u32);

pub struct App {
    pub id: AppId,
    pub title: String,
    pub is_hdr_supported: bool,
}

impl From<network::App> for App {
    fn from(value: network::App) -> Self {
        Self {
            id: AppId(value.id),
            title: value.title,
            is_hdr_supported: value.is_hdr_supported,
        }
    }
}
impl From<App> for api_bindings::App {
    fn from(value: App) -> Self {
        Self {
            app_id: value.id.0,
            title: value.title,
            is_hdr_supported: value.is_hdr_supported,
        }
    }
}

impl Host {
    #[allow(dead_code)]
    pub fn id(&self) -> HostId {
        self.id
    }

    pub async fn modify(&mut self, modify: StorageHostModify) -> Result<(), AppError> {
        let app = self.app.access()?;

        self.cache_storage = None;

        app.storage.modify_host(self.id, modify).await?;

        Ok(())
    }

    pub async fn undetailed_host_cached(&self) -> Result<UndetailedHost, AppError> {
        let app = self.app.access()?;

        let storage = self.storage_host(&app).await?;

        Ok(UndetailedHost {
            host_id: storage.id.0,
            name: storage.cache.name,
            paired: if storage.pair_info.is_some() {
                PairStatus::Paired
            } else {
                PairStatus::NotPaired
            },
            server_state: None,
        })
    }

    async fn use_client<R>(
        &mut self,
        app: &AppInner,
        pairing: bool,
        // app, https_capable, client, host, port, client_info
        f: impl AsyncFnOnce(&mut Self, bool, &mut MoonlightClient, &str, u16, ClientInfo) -> R,
    ) -> Result<R, AppError> {
        let client_unique_id = app.config.moonlight.pair_device_name.clone();
        let host_data = self.storage_host(app).await?;

        let (mut client, https_capable) = if pairing {
            (
                MoonlightClient::with_defaults_long_timeout().map_err(ApiError::RequestClient)?,
                false,
            )
        } else if let Some(pair_info) = host_data.pair_info {
            (
                MoonlightClient::with_certificates(
                    &pair_info.client_private_key,
                    &pair_info.client_certificate,
                    &pair_info.server_certificate,
                )
                .map_err(ApiError::RequestClient)?,
                true,
            )
        } else {
            (
                MoonlightClient::with_defaults().map_err(ApiError::RequestClient)?,
                false,
            )
        };

        let info = ClientInfo {
            unique_id: &client_unique_id,
            uuid: Uuid::new_v4(),
        };

        Ok(f(
            self,
            https_capable,
            &mut client,
            &host_data.address,
            host_data.http_port,
            info,
        )
        .await)
    }
    fn build_hostport(host: &str, port: u16) -> String {
        format!("{host}:{port}")
    }

    async fn storage_host(&self, app: &AppInner) -> Result<StorageHost, AppError> {
        if let Some(host) = self.cache_storage.as_ref() {
            return Ok(host.clone());
        }

        app.storage.get_host(self.id).await
    }

    pub async fn address_port(&self) -> Result<(String, u16), AppError> {
        let app = self.app.access()?;

        let host = app.storage.get_host(self.id).await?;

        Ok((host.address, host.http_port))
    }

    pub async fn pair_info(&self) -> Result<StorageHostPairInfo, AppError> {
        let app = self.app.access()?;

        let host = app.storage.get_host(self.id).await?;

        host.pair_info.ok_or(AppError::HostNotPaired)
    }

    fn is_offline<T>(
        &self,
        result: Result<T, ApiError<<MoonlightClient as RequestClient>::Error>>,
    ) -> Result<Option<T>, AppError> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(ApiError::RequestClient(err)) if err.is_connect() => Ok(None),
            Err(err) => Err(AppError::MoonlightApi(err)),
        }
    }
    // None = Offline
    async fn host_info(&mut self, app: &AppInner) -> Result<Option<HostInfo>, AppError> {
        if let Some(cache) = self.cache_host_info.as_ref() {
            return Ok(Some(cache.clone()));
        }

        self.use_client(
            app,
            false,
            async |this, https_capable, client, host, port, client_info| {
                let mut info = match this.is_offline(
                    host_info(
                        client,
                        false,
                        &Self::build_hostport(host, port),
                        Some(client_info),
                    )
                    .await,
                ) {
                    Ok(Some(value)) => value,
                    err => return err,
                };

                if https_capable {
                    match host_info(
                        client,
                        true,
                        &Self::build_hostport(host, info.https_port),
                        Some(client_info),
                    )
                    .await
                    {
                        Ok(new_info) => {
                            info = new_info;
                        }
                        Err(ApiError::InvalidXmlStatusCode { message: Some(message) })
                            if message.contains("Certificate") =>
                        {
                            // The host likely removed our paired certificate
                            warn!("Host {this:?} has an error related to certificates. This likely happened because the device was removed from sunshine.");
                        }
                        Err(ApiError::RequestClient(err)) if err.is_encryption()  => {
                            // The host likely removed our paired certificate
                            warn!("Host {this:?} has an error related to certificates. This likely happened because the device was removed from sunshine.");
                        }
                        Err(err) => return Err(err.into()),
                    }
                }

                this.cache_host_info = Some(info.clone());

                Ok(Some(info))
            },
        )
        .await?
    }

    pub async fn undetailed_host(&mut self) -> Result<UndetailedHost, AppError> {
        let app = self.app.access()?;

        match self.host_info(&app).await {
            Ok(Some(info)) => {
                let server_state = match ServerState::from_str(&info.state_string) {
                    Ok(state) => Some(state),
                    Err(err) => {
                        warn!(
                            "failed to parse server state of host {self:?}: {:?}, {}",
                            err, info.state_string
                        );

                        None
                    }
                };

                Ok(UndetailedHost {
                    host_id: self.id.0,
                    name: info.host_name,
                    paired: info.pair_status.into(),
                    server_state: server_state.map(HostState::from),
                })
            }
            Ok(None) => {
                let host = self.storage_host(&app).await?;

                let paired = if host.pair_info.is_some() {
                    PairStatus::Paired
                } else {
                    PairStatus::NotPaired
                };

                Ok(UndetailedHost {
                    host_id: self.id.0,
                    name: host.cache.name,
                    paired,
                    server_state: None,
                })
            }
            Err(err) => Err(err),
        }
    }
    pub async fn detailed_host(&mut self) -> Result<DetailedHost, AppError> {
        let app = self.app.access()?;

        let storage = self.storage_host(&app).await?;

        match self.host_info(&app).await {
            Ok(Some(info)) => {
                let server_state = match ServerState::from_str(&info.state_string) {
                    Ok(state) => Some(state),
                    Err(err) => {
                        warn!(
                            "failed to parse server state of host {self:?}: {:?}, {}",
                            err, info.state_string
                        );

                        None
                    }
                };

                Ok(DetailedHost {
                    host_id: self.id.0,
                    name: info.host_name,
                    paired: info.pair_status.into(),
                    server_state: server_state.map(HostState::from),
                    address: storage.address,
                    http_port: storage.http_port,
                    https_port: info.https_port,
                    external_port: info.external_port,
                    version: info.app_version.to_string(),
                    gfe_version: info.gfe_version,
                    unique_id: info.unique_id.to_string(),
                    mac: info.mac.map(|mac| mac.to_string()),
                    local_ip: info.local_ip,
                    current_game: info.current_game,
                    max_luma_pixels_hevc: info.max_luma_pixels_hevc,
                    server_codec_mode_support: info.server_codec_mode_support,
                })
            }
            Ok(None) => {
                let paired = if storage.pair_info.is_some() {
                    PairStatus::Paired
                } else {
                    PairStatus::NotPaired
                };

                Ok(DetailedHost {
                    host_id: self.id.0,
                    name: storage.cache.name,
                    paired,
                    server_state: None,
                    address: storage.address,
                    http_port: storage.http_port,
                    https_port: 0,
                    external_port: 0,
                    version: "Offline".to_string(),
                    gfe_version: "Offline".to_string(),
                    unique_id: "Offline".to_string(),
                    mac: storage.cache.mac.map(|mac| mac.to_string()),
                    local_ip: "Offline".to_string(),
                    current_game: 0,
                    max_luma_pixels_hevc: 0,
                    server_codec_mode_support: 0,
                })
            }
            Err(err) => Err(err),
        }
    }

    #[allow(dead_code)]
    pub async fn is_paired(&mut self) -> Result<PairStatus, AppError> {
        let app = self.app.access()?;

        match self.host_info(&app).await? {
            Some(info) => Ok(info.pair_status.into()),
            None => Ok(PairStatus::NotPaired),
        }
    }

    pub async fn pair(&mut self, pin: PairPin) -> Result<(), AppError> {
        let app = self.app.access()?;

        let info = self.host_info(&app).await?.ok_or(AppError::HostNotFound)?;

        if matches!(info.pair_status.into(), PairStatus::Paired) {
            return Err(AppError::HostPaired);
        }

        let modify = self
            .use_client(
                &app,
                true,
                async |this,_https_capable, client, host, port, client_info| {
                    let auth = generate_new_client()?;

                    let https_address = Self::build_hostport(host, info.https_port);

                    let PairSuccess { server_certificate, mut client } =host_pair(
                        client,
                        &Self::build_hostport(host, port),
                        &https_address,
                        client_info,
                        &auth.private_key,
                        &auth.certificate,
                        &app.config.moonlight.pair_device_name,
                        info.app_version,
                        pin,
                    )
                    .await?;


                    // Store pair info
                    let (name, mac) = match host_info(
                        &mut client,
                        true,
                        &Self::build_hostport(host, info.https_port),
                        Some(client_info),
                    )
                    .await
                    {
                        Ok(info) => {
                            this.cache_host_info = Some(info.clone());

                            (Some(info.host_name), Some(info.mac))
                        },
                        Err(err) => {
                            warn!("Failed to make https request to host {this:?} after pairing completed: {err}");
                            (None, None)
                        },
                    };

                    Ok::<_, AppError>(StorageHostModify {
                        pair_info: Some(Some(StorageHostPairInfo {
                            client_private_key: auth.private_key,
                            client_certificate: auth.certificate,
                            server_certificate,
                        })),
                        cache_name: name,
                        cache_mac: mac,
                        ..Default::default()
                    })
                },
            )
            .await??;

        self.modify(modify).await
    }

    #[allow(dead_code)]
    pub async fn unpair(&self) -> Result<Host, AppError> {
        todo!()
    }

    pub async fn wake(&self) -> Result<(), AppError> {
        let app = self.app.access()?;

        let storage = self.storage_host(&app).await?;

        if let Some(mac) = storage.cache.mac {
            broadcast_magic_packet(mac).await?;
            Ok(())
        } else {
            Err(AppError::HostNotFound)
        }
    }

    pub async fn list_apps(&mut self) -> Result<Vec<App>, AppError> {
        let app = self.app.access()?;

        let info = self.host_info(&app).await?.ok_or(AppError::HostOffline)?;

        self.use_client(
            &app,
            false,
            async |_this, https_capable, client, host, _port, client_info| {
                if !https_capable {
                    return Err(AppError::HostNotPaired);
                }

                let apps = host_app_list(
                    client,
                    &Self::build_hostport(host, info.https_port),
                    client_info,
                )
                .await?;

                let apps = apps.apps.into_iter().map(App::from).collect::<Vec<_>>();

                Ok(apps)
            },
        )
        .await?
    }
    pub async fn app_image(
        &mut self,
        app_id: AppId,
        force_refresh: bool,
    ) -> Result<Bytes, AppError> {
        let app = self.app.access()?;

        let info = self.host_info(&app).await?.ok_or(AppError::HostOffline)?;

        let cache_key = (self.id, app_id);
        if !force_refresh {
            {
                let app_images = app.app_image_cache.read().await;
                if let Some(app_image) = app_images.get(&cache_key) {
                    return Ok(app_image.clone());
                }
            }
        }

        let app_image = self
            .use_client(
                &app,
                false,
                async |_this, https_capable, client, host, _port, client_info| {
                    if !https_capable {
                        return Err(AppError::HostNotPaired);
                    }

                    let image = host_app_box_art(
                        client,
                        &Self::build_hostport(host, info.https_port),
                        client_info,
                        ClientAppBoxArtRequest { app_id: app_id.0 },
                    )
                    .await?;

                    Ok(image)
                },
            )
            .await??;
        let app_image = Bytes::from_owner(app_image);

        {
            let mut app_images = app.app_image_cache.write().await;
            app_images.insert(cache_key, app_image.clone());
        }

        Ok(app_image)
    }

    pub async fn cancel_app(&mut self) -> Result<bool, AppError> {
        let app = self.app.access()?;

        let info = self.host_info(&app).await?.ok_or(AppError::HostOffline)?;

        self.use_client(
            &app,
            false,
            async |_this, https_capable, client, host, _port, client_info| {
                if !https_capable {
                    return Err(AppError::Forbidden);
                }

                let success = host_cancel(
                    client,
                    &Self::build_hostport(host, info.https_port),
                    client_info,
                )
                .await?;

                Ok(success)
            },
        )
        .await?
    }

    pub async fn delete(self) -> Result<(), AppError> {
        let app = self.app.access()?;
        {
            let mut app_images = app.app_image_cache.write().await;
            app_images.retain(|(host_id, _), _| *host_id != self.id);
        }

        drop(app);
        self.delete_no_auth().await
    }
    pub async fn delete_no_auth(self) -> Result<(), AppError> {
        let app = self.app.access()?;

        app.storage.remove_host(self.id).await?;

        Ok(())
    }
}
