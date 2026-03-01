use actix_web::{
    FromRequest, HttpRequest,
    dev::Payload,
    web::Data,
};
use futures::future::{Ready, ready};
use std::pin::Pin;

use crate::app::{
    App, AppError,
    auth::{SessionToken, UserAuth},
    user::{Admin, AuthenticatedUser},
};

pub const COOKIE_SESSION_TOKEN_NAME: &str = "mlSession";

impl FromRequest for UserAuth {
    type Error = AppError;

    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        ready(extract_user_auth(req))
    }
}
fn extract_user_auth(req: &HttpRequest) -> Result<UserAuth, AppError> {
    let app = match req.app_data::<Data<App>>() {
        None => return Err(AppError::AppDestroyed),
        Some(value) => value,
    };

    if let Some(header_auth) = &app.config().web_server.forwarded_header
        && let Some(username) = req.headers().get(&header_auth.username_header)
    {
        let Ok(username) = username.to_str() else {
            return Err(AppError::HeaderAuthMalformed);
        };

        Ok(UserAuth::ForwardedHeaders {
            username: username.to_string(),
        })
    } else if let Some(bearer) = req.headers().get("Authorization") {
        // Look for bearer
        let Ok(bearer) = bearer.to_str() else {
            return Err(AppError::BearerMalformed);
        };

        let token_str = bearer
            .strip_prefix("Bearer")
            .ok_or(AppError::AuthorizationNotBearer)?
            .trim();

        let token = SessionToken::decode(token_str)?;

        Ok(UserAuth::Session(token))
    } else if let Some(cookie) = req.cookie(COOKIE_SESSION_TOKEN_NAME) {
        // Look for cookie
        let token = SessionToken::decode(cookie.value())?;

        Ok(UserAuth::Session(token))
    } else {
        Ok(UserAuth::None)
    }
}

impl FromRequest for AuthenticatedUser {
    type Error = AppError;

    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, payload: &mut Payload) -> Self::Future {
        let app = match req.app_data::<Data<App>>() {
            None => return Box::pin(ready(Err(AppError::AppDestroyed))),
            Some(value) => value,
        };

        let auth_future = UserAuth::from_request(req, payload);

        let app = app.clone();
        Box::pin(async move {
            let auth = auth_future.await?;

            let user = app.user_by_auth(auth).await?;

            Ok(user)
        })
    }
}

impl FromRequest for Admin {
    type Error = AppError;

    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, payload: &mut Payload) -> Self::Future {
        let future = AuthenticatedUser::from_request(req, payload);

        Box::pin(async move {
            let user = future.await?;

            user.into_admin().await
        })
    }
}
