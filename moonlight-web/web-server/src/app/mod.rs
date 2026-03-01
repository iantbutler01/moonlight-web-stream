use std::{
    collections::HashMap,
    io,
    ops::Deref,
    sync::{Arc, Weak},
};

use actix_web::{ResponseError, http::StatusCode, web::Bytes};
use common::config::Config;
use hex::FromHexError;
use moonlight_common::{
    network::{
        ApiError, backend::reqwest::ReqwestClient, request_client::RequestClient,
        request_client::RequestError,
    },
    pair::PairError,
};
use openssl::error::ErrorStack;
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::app::{
    host::{AppId, HostId},
    storage::{Storage, StorageHostAdd, StorageHostCache, create_storage},
};

pub mod host;
pub mod local;
pub mod storage;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("the app got destroyed")]
    AppDestroyed,
    #[error("the host was not found")]
    HostNotFound,
    #[error("the host was already paired")]
    HostPaired,
    #[error("the host must be paired for this action")]
    HostNotPaired,
    #[error("the host was offline, but the action requires that the host is online")]
    HostOffline,
    #[error("the action is not allowed with the current privileges, 403")]
    Forbidden,
    #[error("openssl error occured: {0}")]
    OpenSSL(#[from] ErrorStack),
    #[error("hex error occured: {0}")]
    Hex(#[from] FromHexError),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("moonlight api error: {0}")]
    MoonlightApi(#[from] ApiError<<MoonlightClient as RequestClient>::Error>),
    #[error("pairing error: {0}")]
    Pairing(#[from] PairError<<MoonlightClient as RequestClient>::Error>),
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AppDestroyed => StatusCode::INTERNAL_SERVER_ERROR,
            Self::HostNotFound => StatusCode::NOT_FOUND,
            Self::HostNotPaired => StatusCode::FORBIDDEN,
            Self::HostPaired => StatusCode::NOT_MODIFIED,
            Self::HostOffline => StatusCode::GATEWAY_TIMEOUT,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::OpenSSL(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Hex(_) => StatusCode::BAD_REQUEST,
            Self::MoonlightApi(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Pairing(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Clone)]
struct AppRef {
    inner: Weak<AppInner>,
}

impl AppRef {
    fn access(&self) -> Result<impl Deref<Target = AppInner> + 'static, AppError> {
        Weak::upgrade(&self.inner).ok_or(AppError::AppDestroyed)
    }
}

struct AppInner {
    config: Config,
    storage: Arc<dyn Storage + Send + Sync>,
    app_image_cache: RwLock<HashMap<(HostId, AppId), Bytes>>,
}

pub type MoonlightClient = ReqwestClient;

pub struct App {
    inner: Arc<AppInner>,
}

impl App {
    pub async fn new(config: Config) -> Result<Self, anyhow::Error> {
        let app = AppInner {
            storage: create_storage(config.data_storage.clone()).await?,
            config,
            app_image_cache: Default::default(),
        };

        Ok(Self {
            inner: Arc::new(app),
        })
    }

    fn new_ref(&self) -> AppRef {
        AppRef {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub async fn list_hosts(&self) -> Result<Vec<host::Host>, AppError> {
        let hosts = self
            .inner
            .storage
            .list_hosts()
            .await?
            .into_iter()
            .map(|(host_id, host)| host::Host {
                app: self.new_ref(),
                id: host_id,
                cache_storage: host,
                cache_host_info: None,
            })
            .collect();
        Ok(hosts)
    }

    pub async fn host(&self, host_id: HostId) -> Result<host::Host, AppError> {
        let host = self.inner.storage.get_host(host_id).await?;
        Ok(host::Host {
            app: self.new_ref(),
            id: host.id,
            cache_storage: Some(host),
            cache_host_info: None,
        })
    }

    pub async fn add_host(&self, address: String, http_port: u16) -> Result<host::Host, AppError> {
        let unique_id = self.config().moonlight.pair_device_name.clone();
        let mut client = MoonlightClient::with_defaults().map_err(ApiError::RequestClient)?;
        let info = match moonlight_common::network::host_info(
            &mut client,
            false,
            &format!("{}:{}", address, http_port),
            Some(moonlight_common::network::ClientInfo {
                uuid: Uuid::new_v4(),
                unique_id: &unique_id,
            }),
        )
        .await
        {
            Ok(info) => info,
            Err(ApiError::RequestClient(err)) if err.is_connect() => {
                return Err(AppError::HostNotFound);
            }
            Err(err) => return Err(err.into()),
        };

        let host = self
            .inner
            .storage
            .add_host(StorageHostAdd {
                address,
                http_port,
                pair_info: None,
                cache: StorageHostCache {
                    name: info.host_name,
                    mac: info.mac,
                },
            })
            .await?;

        Ok(host::Host {
            app: self.new_ref(),
            id: host.id,
            cache_storage: Some(host),
            cache_host_info: None,
        })
    }
}
