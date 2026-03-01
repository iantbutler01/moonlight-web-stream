use std::sync::Arc;

use async_trait::async_trait;
use common::config::StorageConfig;
use pem::Pem;

use crate::app::{AppError, host::HostId, storage::json::JsonStorage};

pub mod json;

pub async fn create_storage(
    config: StorageConfig,
) -> Result<Arc<dyn Storage + Send + Sync>, anyhow::Error> {
    match config {
        StorageConfig::Json {
            path,
            session_expiration_check_interval,
        } => {
            let storage = JsonStorage::load(path.into(), session_expiration_check_interval).await?;
            Ok(storage)
        }
    }
}

#[derive(Clone)]
pub struct StorageHost {
    pub id: HostId,
    pub address: String,
    pub http_port: u16,
    pub pair_info: Option<StorageHostPairInfo>,
}

#[derive(Clone)]
pub struct StorageHostAdd {
    pub address: String,
    pub http_port: u16,
    pub pair_info: Option<StorageHostPairInfo>,
}

#[derive(Clone)]
pub struct StorageHostPairInfo {
    pub client_private_key: Pem,
    pub client_certificate: Pem,
    pub server_certificate: Pem,
}

#[derive(Default, Clone)]
pub struct StorageHostModify {
    pub address: Option<String>,
    pub http_port: Option<u16>,
    pub pair_info: Option<Option<StorageHostPairInfo>>,
}

#[async_trait]
pub trait Storage {
    async fn add_host(&self, host: StorageHostAdd) -> Result<StorageHost, AppError>;
    async fn modify_host(&self, host_id: HostId, host: StorageHostModify) -> Result<(), AppError>;
    async fn get_host(&self, host_id: HostId) -> Result<StorageHost, AppError>;
    async fn list_hosts(&self) -> Result<Vec<(HostId, Option<StorageHost>)>, AppError>;
}
