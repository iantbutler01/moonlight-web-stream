use std::{collections::HashMap, io::ErrorKind, path::PathBuf, sync::Arc, time::Duration};

use anyhow::anyhow;
use async_trait::async_trait;
use futures::future::join_all;
use log::error;
use openssl::rand::rand_bytes;
use tokio::{
    fs, spawn,
    sync::{
        RwLock,
        mpsc::{self, Receiver, Sender, error::TrySendError},
    },
    task::JoinHandle,
};

use crate::app::{
    AppError,
    host::HostId,
    storage::{
        Storage, StorageHost, StorageHostAdd, StorageHostCache, StorageHostModify,
        StorageHostPairInfo,
        json::versions::{Json, V2, V2Host, V2HostCache, V2HostPairInfo, migrate_to_latest},
    },
};

mod serde_helpers;
mod versions;

pub struct JsonStorage {
    file: PathBuf,
    store_sender: Sender<()>,
    _lifecycle_task: JoinHandle<()>,
    hosts: RwLock<HashMap<u32, RwLock<V2Host>>>,
}

impl Drop for JsonStorage {
    fn drop(&mut self) {
        self._lifecycle_task.abort();
    }
}

impl JsonStorage {
    pub async fn load(
        file: PathBuf,
        _session_expiration_check_interval: Duration,
    ) -> Result<Arc<Self>, anyhow::Error> {
        let (store_sender, store_receiver) = mpsc::channel(1);
        let lifecycle_task = spawn(async move {});

        let this = Arc::new(Self {
            file,
            store_sender,
            _lifecycle_task: lifecycle_task,
            hosts: Default::default(),
        });

        this.load_internal().await?;

        spawn({
            let this = this.clone();
            async move { file_writer(store_receiver, this).await }
        });

        Ok(this)
    }

    fn force_write(&self) {
        if let Err(TrySendError::Closed(_)) = self.store_sender.try_send(()) {
            error!("Failed to save data because the writer task closed!");
        }
    }

    async fn load_internal(&self) -> Result<(), anyhow::Error> {
        let text = match fs::read_to_string(&self.file).await {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(anyhow!("Failed to read data: {err:?}")),
        };

        let json = match serde_json::from_str::<Json>(&text) {
            Ok(value) => value,
            Err(err) => {
                let error = serde_json::from_str::<V2>(&text)
                    .err()
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "none".to_string());

                return Err(anyhow!(
                    "Failed to deserialize data as json: {err}, Version specific error: {error}"
                ));
            }
        };

        let data = migrate_to_latest(json)?;
        let mut hosts = self.hosts.write().await;
        *hosts = data
            .hosts
            .into_iter()
            .map(|(id, host)| (id, RwLock::new(host)))
            .collect();

        Ok(())
    }

    async fn store(&self) {
        let hosts = self.hosts.read().await;
        let mut hosts_json = HashMap::new();
        for (key, value) in hosts.iter() {
            let value = value.read().await;
            hosts_json.insert(*key, (*value).clone());
        }

        let json = Json::V2(V2 { hosts: hosts_json });

        let text = match serde_json::to_string_pretty(&json) {
            Ok(text) => text,
            Err(err) => {
                error!("Failed to serialize data to json: {err:?}");
                return;
            }
        };

        if let Err(err) = fs::write(&self.file, text).await {
            error!("Failed to write data to file: {err:?}");
        }
    }
}

async fn file_writer(mut store_receiver: Receiver<()>, json: Arc<JsonStorage>) {
    loop {
        if store_receiver.recv().await.is_none() {
            return;
        }
        json.store().await;
    }
}

fn host_from_json(host_id: HostId, host: &V2Host) -> StorageHost {
    StorageHost {
        id: host_id,
        address: host.address.clone(),
        http_port: host.http_port,
        pair_info: host.pair_info.clone().map(|pair_info| StorageHostPairInfo {
            client_certificate: pair_info.client_certificate,
            client_private_key: pair_info.client_private_key,
            server_certificate: pair_info.server_certificate,
        }),
        cache: StorageHostCache {
            name: host.cache.name.clone(),
            mac: host.cache.mac,
        },
    }
}

#[async_trait]
impl Storage for JsonStorage {
    async fn add_host(&self, host: StorageHostAdd) -> Result<StorageHost, AppError> {
        let host = V2Host {
            address: host.address,
            http_port: host.http_port,
            pair_info: host.pair_info.map(|pair_info| V2HostPairInfo {
                client_private_key: pair_info.client_private_key,
                client_certificate: pair_info.client_certificate,
                server_certificate: pair_info.server_certificate,
            }),
            cache: V2HostCache {
                name: host.cache.name,
                mac: host.cache.mac,
            },
        };

        let mut hosts = self.hosts.write().await;
        let mut id;
        loop {
            let mut id_bytes = [0u8; 4];
            rand_bytes(&mut id_bytes)?;
            id = u32::from_be_bytes(id_bytes);
            if !hosts.contains_key(&id) {
                break;
            }
        }
        hosts.insert(id, RwLock::new(host.clone()));
        drop(hosts);

        self.force_write();

        Ok(StorageHost {
            id: HostId(id),
            address: host.address,
            http_port: host.http_port,
            pair_info: host.pair_info.map(|pair_info| StorageHostPairInfo {
                client_private_key: pair_info.client_private_key,
                client_certificate: pair_info.client_certificate,
                server_certificate: pair_info.server_certificate,
            }),
            cache: StorageHostCache {
                name: host.cache.name,
                mac: host.cache.mac,
            },
        })
    }

    async fn modify_host(
        &self,
        host_id: HostId,
        modify: StorageHostModify,
    ) -> Result<(), AppError> {
        let hosts = self.hosts.read().await;
        let host = hosts.get(&host_id.0).ok_or(AppError::HostNotFound)?;
        let mut host = host.write().await;

        if let Some(new_address) = modify.address {
            host.address = new_address;
        }
        if let Some(new_http_port) = modify.http_port {
            host.http_port = new_http_port;
        }
        if let Some(new_pair_info) = modify.pair_info {
            host.pair_info = new_pair_info.map(|pair_info| V2HostPairInfo {
                client_private_key: pair_info.client_private_key,
                client_certificate: pair_info.client_certificate,
                server_certificate: pair_info.server_certificate,
            });
        }
        if let Some(new_name) = modify.cache_name {
            host.cache.name = new_name;
        }
        if let Some(new_mac) = modify.cache_mac {
            host.cache.mac = new_mac;
        }

        drop(host);
        drop(hosts);
        self.force_write();
        Ok(())
    }

    async fn get_host(&self, host_id: HostId) -> Result<StorageHost, AppError> {
        let hosts = self.hosts.read().await;
        let host = hosts.get(&host_id.0).ok_or(AppError::HostNotFound)?;
        let host = host.read().await;
        Ok(host_from_json(host_id, &host))
    }

    async fn remove_host(&self, host_id: HostId) -> Result<(), AppError> {
        let mut hosts = self.hosts.write().await;
        let result = match hosts.remove(&host_id.0) {
            None => Err(AppError::HostNotFound),
            Some(_) => Ok(()),
        };
        drop(hosts);
        self.force_write();
        result
    }

    async fn list_hosts(&self) -> Result<Vec<(HostId, Option<StorageHost>)>, AppError> {
        let hosts = self.hosts.read().await;
        let futures = hosts.iter().map(|(id, host)| {
            let id = *id;
            async move {
                let host = host.read().await.clone();
                (HostId(id), Some(host_from_json(HostId(id), &host)))
            }
        });
        Ok(join_all(futures).await)
    }
}
