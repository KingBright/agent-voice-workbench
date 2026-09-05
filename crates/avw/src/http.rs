use crate::service::{App, Paging};
use avw_core::{Error, types::AssetId, audio::MAX_AUDIO_BYTES};
use axum::{Router, Json, body::Bytes, extract::{DefaultBodyLimit, Path, Query, State, Request}, http::{StatusCode, header}, middleware::{self, Next}, response::{IntoResponse, Response}, routing::{get, post}};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

pub struct ApiError(pub Error);
impl From<Error> for ApiError { fn from(error: Error) -> Self { Self(error) } }
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 { Error::Invalid(_) | Error::Json(_) | Error::Wav(_) => StatusCode::BAD_REQUEST, Error::NotFound(_) => StatusCode::NOT_FOUND, Error::Conflict(_) => StatusCode::CONFLICT, Error::Capacity(_) => StatusCode::TOO_MANY_REQUESTS, Error::Unsupported(_) => StatusCode::UNPROCESSABLE_ENTITY, Error::Cancelled => StatusCode::CONFLICT, Error::Deadline => StatusCode::REQUEST_TIMEOUT, _ => StatusCode::INTERNAL_SERVER_ERROR };
        (status, Json(json!({"error":self.0.failure()}))).into_response()
    }
}
#[derive(Clone)]
pub struct Security { token:Option<Arc<str>>, slots:Arc<Semaphore> }
impl Security {
    pub fn new(token:Option<String>) -> avw_core::Result<Self> {
        if token.as_ref().is_some_and(|s| s.len() < 32 || s.len() > 4096 || !s.is_ascii() || s.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control())) { return Err(Error::Invalid("AVW_TOKEN must be 32..=4096 visible ASCII bytes without whitespace".into())); }
        Ok(Self { token:token.map(Arc::from), slots:Arc::new(Semaphore::new(4)) })
    }
}
async fn guard(State(security):State<Security>, request:Request, next:Next) -> Response {
    // This is a local agent API, not a browser application. Reject all browser origins.
    let forbidden = || (StatusCode::FORBIDDEN, Json(json!({"error":{"code":"forbidden","message":"origin or host rejected"}}))).into_response();
    if request.headers().contains_key(header::ORIGIN) { return forbidden(); }
    let authority = request.headers().get(header::HOST).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<axum::http::uri::Authority>().ok());
    if !authority.is_some_and(|a| matches!(a.host(), "localhost" | "127.0.0.1" | "[::1]" | "::1")) { return forbidden(); }
    if let Some(expected) = &security.token {
        let supplied = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
        if supplied.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() != 1 {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error":{"code":"unauthorized","message":"valid bearer token required"}}))).into_response();
        }
    }
    let Ok(_permit) = security.slots.clone().try_acquire_owned() else { return ApiError(Error::Capacity("HTTP concurrency limit reached".into())).into_response(); };
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, axum::http::HeaderValue::from_static("nosniff"));
    response
}
async fn blocking<T, F>(work:F) -> Result<T,ApiError> where T:Send+'static, F:FnOnce()->avw_core::Result<T>+Send+'static {
    tokio::task::spawn_blocking(work).await.map_err(|_| ApiError(Error::Internal("request worker failed".into())))?.map_err(ApiError)
}
pub fn router(app:App, security:Security) -> Router {
    Router::new()
        .route("/healthz",get(health))
        .route("/v1/capabilities",get(capabilities))
        .route("/v1/jobs",post(submit).get(jobs))
        .route("/v1/jobs/{id}",get(job))
        .route("/v1/jobs/{id}/cancel",post(cancel))
        .route("/v1/jobs/{id}/events",get(events))
        .route("/v1/tools/{name}",post(call))
        .route("/v1/assets",post(import_audio).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)))
        .route("/v1/assets/{id}",get(asset_meta))
        .route("/v1/assets/{id}/content",get(asset_content))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"error":{"code":"not_found","message":"route not found"}}))) })
        .layer(DefaultBodyLimit::max(1024*1024))
        .layer(middleware::from_fn_with_state(security,guard)).with_state(app)
}
async fn health(State(app):State<App>) -> Result<Json<Value>,ApiError> { app.runtime.health()?; Ok(Json(json!({"status":"ok","worker":"ready"}))) }
async fn capabilities(State(app):State<App>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || Ok(app.registry.capabilities(app.device))).await?)) }
async fn call(State(app):State<App>,Path(name):Path<String>,Json(args):Json<Value>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch(&name,args)).await?)) }
async fn submit(State(app):State<App>,Json(args):Json<Value>) -> Result<(StatusCode,Json<Value>),ApiError> { Ok((StatusCode::ACCEPTED,Json(blocking(move || app.dispatch("jobs_submit",args)).await?))) }
async fn jobs(State(app):State<App>,Query(p):Query<Paging>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch("jobs_list",json!({"after":p.after,"limit":p.limit}))).await?)) }
async fn job(State(app):State<App>,Path(id):Path<String>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch("jobs_get",json!({"job_id":id}))).await?)) }
async fn cancel(State(app):State<App>,Path(id):Path<String>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch("jobs_cancel",json!({"job_id":id}))).await?)) }
async fn events(State(app):State<App>,Path(id):Path<String>,Query(p):Query<Paging>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch("jobs_events",json!({"job_id":id,"after":p.after,"limit":p.limit}))).await?)) }
async fn import_audio(State(app):State<App>,body:Bytes) -> Result<(StatusCode,Json<Value>),ApiError> {
    let value = blocking(move || Ok(serde_json::to_value(app.runtime.artifacts().import_wav(&body)?)?)).await?;
    Ok((StatusCode::CREATED,Json(value)))
}
async fn asset_meta(State(app):State<App>,Path(id):Path<String>) -> Result<Json<Value>,ApiError> { Ok(Json(blocking(move || app.dispatch("assets_get",json!({"asset_id":id}))).await?)) }
async fn asset_content(State(app):State<App>,Path(id):Path<String>) -> Result<Response,ApiError> {
    let (meta,bytes) = blocking(move || {
        let id=AssetId::try_from(id).map_err(Error::Invalid)?;
        app.runtime.artifacts().read(&id)
    }).await?;
    // Metadata is local data, but still validate it before inserting a header.
    let content_type = axum::http::HeaderValue::from_str(&meta.media_type).map_err(|_| ApiError(Error::Integrity("invalid asset media type".into())))?;
    let mut response=bytes.into_response(); response.headers_mut().insert(header::CONTENT_TYPE,content_type); Ok(response)
}
pub async fn serve(app:App, listen:SocketAddr, security:Security) -> anyhow::Result<()> {
    if !listen.ip().is_loopback() { anyhow::bail!("REST binds to loopback only. Use an authenticated tunnel for another machine."); }
    let listener=tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(address=%listener.local_addr()?,"local voice workbench listening");
    let runtime=app.runtime.clone();
    let result=axum::serve(listener,router(app,security)).with_graceful_shutdown(async { let _=tokio::signal::ctrl_c().await; }).await;
    tokio::task::spawn_blocking(move || runtime.shutdown()).await??; result?; Ok(())
}
#[cfg(test)]
mod tests {
    use super::*; use tower::ServiceExt;
    #[tokio::test] async fn rejects_browser_origin() {
        let d=tempfile::tempdir().unwrap(); let app=App::open(d.path(),avw_core::types::Device::Cpu).unwrap();
        let response=router(app,Security::new(None).unwrap()).oneshot(Request::builder().uri("/healthz").header("host","localhost").header("origin","https://evil.example").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
    }
    #[tokio::test] async fn enforces_bearer() {
        let d=tempfile::tempdir().unwrap(); let app=App::open(d.path(),avw_core::types::Device::Cpu).unwrap();
        let response=router(app,Security::new(Some("x".repeat(32))).unwrap()).oneshot(Request::builder().uri("/healthz").header("host","localhost").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    }
    #[test] fn rejects_short_token() { assert!(Security::new(Some("abc".into())).is_err()); }
}
