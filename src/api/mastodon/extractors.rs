use axum::{
    extract::{FromRequest, Multipart},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

/// Accepts JSON body, multipart/form-data body, or application/x-www-form-urlencoded body.
/// Mirrors Rails' transparent parameter handling.
pub struct FormOrJson<T>(pub T);

impl<T, S> FromRequest<S> for FormOrJson<T>
where
    T: serde::de::DeserializeOwned + Send + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("application/json") {
            Json::<T>::from_request(req, state)
                .await
                .map(|Json(v)| FormOrJson(v))
                .map_err(IntoResponse::into_response)
        } else if content_type.contains("multipart/form-data") {
            let mut multipart = Multipart::from_request(req, state)
                .await
                .map_err(IntoResponse::into_response)?;
            let mut pairs: Vec<(String, String)> = Vec::new();
            while let Some(field) = multipart
                .next_field()
                .await
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())?
            {
                let name = field.name().unwrap_or("").to_string();
                let value = field.text().await.map_err(|e| {
                    (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response()
                })?;
                pairs.push((name, value));
            }
            let encoded = serde_urlencoded::to_string(&pairs)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())?;
            serde_urlencoded::from_str::<T>(&encoded)
                .map(FormOrJson)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())
        } else {
            let bytes = axum::body::Bytes::from_request(req, state)
                .await
                .map_err(IntoResponse::into_response)?;
            serde_urlencoded::from_bytes::<T>(&bytes)
                .map(FormOrJson)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())
        }
    }
}

/// Accepts a JSON body OR URL query parameters (using serde_qs for bracket-notation
/// arrays like `keys[0]=...`). Used for POST endpoints where clients like Nicolium
/// pass params in the query string instead of the body.
pub struct QueryOrJson<T>(pub T);

impl<T, S> FromRequest<S> for QueryOrJson<T>
where
    T: serde::de::DeserializeOwned + Send + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("application/json") {
            let (parts, body) = req.into_parts();
            let req = axum::extract::Request::from_parts(parts, body);
            Json::<T>::from_request(req, state)
                .await
                .map(|Json(v)| QueryOrJson(v))
                .map_err(IntoResponse::into_response)
        } else {
            let (parts, _body) = req.into_parts();
            let query = parts.uri.query().unwrap_or("");
            serde_qs::from_str::<T>(query)
                .map(QueryOrJson)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())
        }
    }
}
