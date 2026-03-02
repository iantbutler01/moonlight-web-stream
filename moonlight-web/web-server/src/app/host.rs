use std::fmt::{Debug, Formatter};

use common::api_bindings::{self, PairStatus};
use log::warn;
use moonlight_common::{
    PairPin,
    network::{
        self, ApiError, ClientInfo, HostInfo, host_app_list, host_cancel, host_info,
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
    pub fn id(&self) -> HostId {
        self.id
    }

    pub async fn modify(&mut self, modify: StorageHostModify) -> Result<(), AppError> {
        let app = self.app.access()?;

        self.cache_storage = None;

        app.storage.modify_host(self.id, modify).await?;

        Ok(())
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


                    // Refresh host info cache after pairing when possible.
                    if let Err(err) = host_info(
                        &mut client,
                        true,
                        &Self::build_hostport(host, info.https_port),
                        Some(client_info),
                    )
                    .await
                    {
                        warn!(
                            "Failed to make https request to host {this:?} after pairing completed: {err}"
                        );
                    }

                    Ok::<_, AppError>(StorageHostModify {
                        pair_info: Some(Some(StorageHostPairInfo {
                            client_private_key: auth.private_key,
                            client_certificate: auth.certificate,
                            server_certificate,
                        })),
                        ..Default::default()
                    })
                },
            )
            .await??;

        self.modify(modify).await
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
}
