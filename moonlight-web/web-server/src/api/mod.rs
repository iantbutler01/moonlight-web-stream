use actix_web::{
    HttpRequest, http::header,
    HttpResponse, delete,
    dev::HttpServiceFactory,
    get,
    post, services,
    web::{self, Data, Json, Query},
};
use futures::future::try_join_all;
use log::warn;
use moonlight_common::PairPin;
use tokio::spawn;
use sha2::{Digest, Sha256};

use crate::{
    api::response_streaming::StreamedResponse,
    app::{
        App, AppError,
        host::{AppId, HostId},
    },
};
use common::api_bindings::{
    self, DeleteHostQuery, GetAppImageQuery, GetAppsQuery, GetAppsResponse, GetHostQuery,
    GetHostResponse, GetHostsResponse, PostHostRequest, PostHostResponse, PostPairRequest,
    PostPairResponse1, PostPairResponse2, PostWakeUpRequest, UndetailedHost,
};

pub mod stream;

pub mod response_streaming;

#[get("/hosts")]
async fn list_hosts(
    app: Data<App>,
) -> Result<StreamedResponse<GetHostsResponse, UndetailedHost>, AppError> {
    let (mut stream_response, stream_sender) =
        StreamedResponse::new(GetHostsResponse { hosts: Vec::new() });

    let hosts = app.list_hosts().await?;

    // Try join all because storage should always work, the actual host info will be send using response streaming
    let undetailed_hosts = try_join_all(hosts.into_iter().map(move |mut host| {
        let stream_sender = stream_sender.clone();

        async move {
            // First query db
            let undetailed_cache = host.undetailed_host_cached().await;

            // Then send http request now
            spawn(async move {
                let undetailed = match host.undetailed_host().await {
                    Ok(value) => value,
                    Err(err) => {
                        warn!("Failed to get undetailed host of {host:?}: {err}");
                        return;
                    }
                };

                if let Err(err) = stream_sender.send(undetailed).await {
                    warn!(
                        "Failed to send back undetailed host data using response streaming: {err}"
                    );
                }
            });

            undetailed_cache
        }
    }))
    .await?;

    stream_response.set_initial(GetHostsResponse {
        hosts: undetailed_hosts,
    });

    Ok(stream_response)
}

#[get("/host")]
async fn get_host(
    app: Data<App>,
    Query(query): Query<GetHostQuery>,
) -> Result<Json<GetHostResponse>, AppError> {
    let host_id = HostId(query.host_id);

    let mut host = app.host(host_id).await?;

    let detailed = host.detailed_host().await?;

    Ok(Json(GetHostResponse { host: detailed }))
}

#[post("/host")]
async fn post_host(
    app: Data<App>,
    Json(request): Json<PostHostRequest>,
) -> Result<Json<PostHostResponse>, AppError> {
    let mut host = app
        .add_host(
            request.address,
            request
                .http_port
                .unwrap_or(app.config().moonlight.default_http_port),
        )
        .await?;

    Ok(Json(PostHostResponse {
        host: host.detailed_host().await?,
    }))
}

#[delete("/host")]
async fn delete_host(
    app: Data<App>,
    Query(query): Query<DeleteHostQuery>,
) -> Result<HttpResponse, AppError> {
    let host_id = HostId(query.host_id);

    app.host(host_id).await?.delete().await?;

    Ok(HttpResponse::Ok().finish())
}

#[post("/pair")]
async fn pair_host(
    app: Data<App>,
    Json(request): Json<PostPairRequest>,
) -> Result<StreamedResponse<PostPairResponse1, PostPairResponse2>, AppError> {
    let host_id = HostId(request.host_id);

    let mut host = app.host(host_id).await?;

    let pin = PairPin::generate()?;

    let (stream_response, stream_sender) =
        StreamedResponse::new(PostPairResponse1::Pin(pin.to_string()));

    spawn(async move {
        let result = host.pair(pin).await;

        let result = match result {
            Ok(()) => host.detailed_host().await,
            Err(err) => Err(err),
        };

        match result {
            Ok(detailed_host) => {
                if let Err(err) = stream_sender
                    .send(PostPairResponse2::Paired(detailed_host))
                    .await
                {
                    warn!("Failed to send pair success: {err}");
                }
            }
            Err(err) => {
                warn!("Failed to pair host: {err}");
                if let Err(err) = stream_sender.send(PostPairResponse2::PairError).await {
                    warn!("Failed to send pair failure: {err}");
                }
            }
        }
    });

    Ok(stream_response)
}

#[post("/host/wake")]
async fn wake_host(
    app: Data<App>,
    Json(request): Json<PostWakeUpRequest>,
) -> Result<HttpResponse, AppError> {
    let host_id = HostId(request.host_id);

    let host = app.host(host_id).await?;

    host.wake().await?;

    Ok(HttpResponse::Ok().finish())
}

#[get("/apps")]
async fn get_apps(
    app: Data<App>,
    Query(query): Query<GetAppsQuery>,
) -> Result<Json<GetAppsResponse>, AppError> {
    let host_id = HostId(query.host_id);

    let mut host = app.host(host_id).await?;

    let apps = host.list_apps().await?;

    Ok(Json(GetAppsResponse {
        apps: apps
            .into_iter()
            .map(|app| api_bindings::App {
                app_id: app.id.0,
                title: app.title,
                is_hdr_supported: app.is_hdr_supported,
            })
            .collect(),
    }))
}

#[get("/app/image")]
async fn get_app_image(
    app: Data<App>,
    Query(query): Query<GetAppImageQuery>,
    req: HttpRequest,
) -> Result<HttpResponse, AppError> {
    let host_id = HostId(query.host_id);
    let app_id = AppId(query.app_id);

    let mut host = app.host(host_id).await?;

    let image = host.app_image(app_id, query.force_refresh).await?;

    let mut hasher = Sha256::new();
    hasher.update(&image);
    let etag = format!("\"{:x}\"", hasher.finalize());

    let cache_control = "private, no-cache, must-revalidate";

    if let Some(if_none_match) = req.headers().get(header::IF_NONE_MATCH) {
        if if_none_match.to_str().ok() == Some(&etag) && query.force_refresh == false {
            return Ok(
                HttpResponse::NotModified()
                    .insert_header((header::ETAG, etag))
                    .insert_header((header::CACHE_CONTROL, cache_control))
                    .finish()
            );
        }
    }

    Ok(
        HttpResponse::Ok()
            .insert_header((header::ETAG, etag))
            .insert_header((header::CACHE_CONTROL, cache_control))
            .body(image)
    )
}

pub fn api_service() -> impl HttpServiceFactory {
    web::scope("/api")
        .service(services![
            // -- Host
            list_hosts,
            get_host,
            post_host,
            wake_host,
            delete_host,
            pair_host,
            get_apps,
            get_app_image,
        ])
        .service(services![
            // -- Stream
            stream::start_host,
            stream::cancel_host,
        ])
}
