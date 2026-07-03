pub mod nodeinfo;
pub mod webfinger;

use crate::state::AppState;
use axum::{routing::get, Router};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/.well-known/webfinger", get(webfinger::webfinger))
        .route("/.well-known/nodeinfo", get(nodeinfo::nodeinfo_links))
        .route("/nodeinfo/2.0", get(nodeinfo::nodeinfo))
        .route("/.well-known/host-meta", get(host_meta))
}

async fn host_meta(
    axum::extract::Extension(crate::middleware::ResolvedInstance(instance)): axum::extract::Extension<crate::middleware::ResolvedInstance>,
) -> axum::response::Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="lrdd" template="https://{}/.well-known/webfinger?resource={{uri}}"/>
</XRD>"#,
        instance.domain
    );
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/xrd+xml; charset=utf-8",
        )],
        xml,
    )
        .into_response()
}

use axum::response::IntoResponse;
