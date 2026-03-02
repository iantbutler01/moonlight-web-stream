use actix_web::{
    HttpResponse,
    dev::HttpServiceFactory,
    get, post, services,
    web::{self, Data, Json},
};

use crate::app::{
    App,
    local::{self, LocalEnsureError, LocalFailureCode as LocalFailureKind},
};
use common::api_bindings::{
    GetLocalBootstrapResponse, GetLocalStatusResponse, LocalErrorResponse, LocalFailureCode,
    LocalStreamCapabilities,
};

pub mod stream;

#[get("/local/status")]
async fn local_status(app: Data<App>) -> Json<GetLocalStatusResponse> {
    let status = local::get_local_status(&app).await;

    Json(GetLocalStatusResponse {
        ready: status.ready,
        host_id: status.host_id,
        app_id: status.app_id,
        status_text: status.status_text,
        capabilities: LocalStreamCapabilities {
            can_take_over: status.can_take_over,
            observe_mode_default: status.observe_mode_default,
        },
        failure: status.failure.map(into_api_local_failure_code),
    })
}

#[post("/local/ensure_ready")]
async fn local_ensure_ready(app: Data<App>) -> HttpResponse {
    local_bootstrap_response(app).await
}

#[get("/local/bootstrap")]
async fn local_bootstrap(app: Data<App>) -> HttpResponse {
    local_bootstrap_response(app).await
}

async fn local_bootstrap_response(app: Data<App>) -> HttpResponse {
    match local::ensure_local_bootstrap(&app).await {
        Ok(bootstrap) => HttpResponse::Ok().json(GetLocalBootstrapResponse {
            host_id: bootstrap.host_id,
            app_id: bootstrap.app_id,
            status_text: bootstrap.status_text,
            capabilities: LocalStreamCapabilities {
                can_take_over: bootstrap.can_take_over,
                observe_mode_default: bootstrap.observe_mode_default,
            },
        }),
        Err(error) => local_error_response(error),
    }
}

fn local_error_response(error: LocalEnsureError) -> HttpResponse {
    let mut status_code = if error.retryable {
        HttpResponse::ServiceUnavailable()
    } else {
        HttpResponse::InternalServerError()
    };

    status_code.json(LocalErrorResponse {
        code: into_api_local_failure_code(error.code),
        status_text: error.status_text,
        detail: error.detail,
    })
}

fn into_api_local_failure_code(value: LocalFailureKind) -> LocalFailureCode {
    match value {
        LocalFailureKind::SunshineNotReady => LocalFailureCode::SunshineNotReady,
        LocalFailureKind::AssociationFailed => LocalFailureCode::AssociationFailed,
        LocalFailureKind::PairingFailed => LocalFailureCode::PairingFailed,
        LocalFailureKind::DesktopAppNotFound => LocalFailureCode::DesktopAppNotFound,
        LocalFailureKind::StreamBackendUnhealthy => LocalFailureCode::StreamBackendUnhealthy,
    }
}

pub fn api_service() -> impl HttpServiceFactory {
    web::scope("/api")
        .service(services![local_status, local_ensure_ready, local_bootstrap,])
        .service(services![stream::start_host, stream::cancel_host,])
}
