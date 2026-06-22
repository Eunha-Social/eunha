use axum::{
    extract::{Extension, Multipart, Path, State},
    Json,
};
use image::imageops::FilterType;
use img_parts::ImageEXIF;
use crate::{
    error::{AppError, AppResult},
    middleware::AuthenticatedUser,
    state::AppState,
};
use super::{convert::media_from_db, types::MediaAttachment};

// Mastodon's small thumbnail pixel limit (≈640×360 at 16:9)
const SMALL_PIXELS: u32 = 230_400;

// ── POST /api/v1/media, POST /api/v2/media ────────────────────────────────

pub async fn upload_media(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    mut multipart: Multipart,
) -> AppResult<Json<MediaAttachment>> {
    auth.require_scope("write:media")?;
    let mut file_field: Option<(String, String, Vec<u8>)> = None;
    let mut description: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| AppError::Unprocessable(e.to_string()))? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("upload").to_string();
                let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();
                let data = field.bytes().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                file_field = Some((filename, content_type, data.to_vec()));
            }
            "description" => {
                let text = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                description = Some(text);
            }
            _ => {}
        }
    }

    let (_, content_type, data) = file_field.ok_or_else(|| AppError::Unprocessable("missing file field".into()))?;
    let media_type = classify_media_type(&content_type);
    let media_id = crate::snowflake::next_id();
    // Compute filename and S3 key using Mastodon Paperclip convention (id-based path)
    let file_ext = crate::media::ext_for_content_type(&content_type);
    let file_filename = format!("original.{}", file_ext);
    let file_key = format!("media_attachments/files/{}/original/{}", crate::media::int_to_path(media_id), file_filename);

    let data = strip_exif(&data, &content_type);
    state.storage.store(&data, &file_key, &content_type).await?;

    let (file_meta, blurhash, thumbnail_file_name) = if media_type == "image" || media_type == "gifv" {
        match process_image(&data, &content_type) {
            Some((orig_dim, small_bytes, small_dim, bh)) => {
                let small_filename = format!("small.{}", file_ext);
                let small_key = format!("media_attachments/files/{}/small/{}", crate::media::int_to_path(media_id), small_filename);
                state.storage.store(&small_bytes, &small_key, &content_type).await?;
                let meta = serde_json::json!({
                    "original": orig_dim,
                    "small": small_dim,
                });
                (Some(meta), Some(bh), Some(small_filename))
            }
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };

    let file_size = data.len() as i32;
    let attachment = sqlx::query_as!(
        crate::db::models::MediaAttachment,
        r#"INSERT INTO media_attachments
             (id, account_id, "type", file_file_name, file_content_type, file_file_size, thumbnail_file_name, description, file_meta, blurhash)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           RETURNING *"#,
        media_id,
        auth.account_id,
        media_type_int(media_type),
        file_filename,
        content_type,
        file_size,
        thumbnail_file_name,
        description,
        file_meta,
        blurhash,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(Json(media_from_db(&attachment)))
}

/// Decode image, compute original + small dimensions and blurhash.
/// Returns (orig_dim, small_jpeg_bytes, small_dim, blurhash).
fn process_image(data: &[u8], _content_type: &str) -> Option<(serde_json::Value, Vec<u8>, serde_json::Value, String)> {
    let img = image::load_from_memory(data).ok()?;
    let (ow, oh) = (img.width(), img.height());
    let orig_dim = image_dim_json(ow, oh);

    // Compute blurhash from original (4×4 components, matching Mastodon)
    let rgba = img.to_rgba8();
    let bh = blurhash::encode(4, 4, ow, oh, rgba.as_raw()).ok()?;

    // Resize to small: scale down only if total pixels exceed SMALL_PIXELS
    let small_img = if ow * oh > SMALL_PIXELS {
        let scale = (SMALL_PIXELS as f64 / (ow * oh) as f64).sqrt();
        let sw = ((ow as f64 * scale).round() as u32).max(1);
        let sh = ((oh as f64 * scale).round() as u32).max(1);
        img.resize(sw, sh, FilterType::Lanczos3)
    } else {
        img
    };
    let (sw, sh) = (small_img.width(), small_img.height());
    let small_dim = image_dim_json(sw, sh);

    // Encode small as JPEG
    let mut small_bytes = Vec::new();
    small_img
        .write_to(&mut std::io::Cursor::new(&mut small_bytes), image::ImageFormat::Jpeg)
        .ok()?;

    Some((orig_dim, small_bytes, small_dim, bh))
}

fn image_dim_json(w: u32, h: u32) -> serde_json::Value {
    serde_json::json!({
        "width": w,
        "height": h,
        "size": format!("{}x{}", w, h),
        "aspect": w as f64 / h as f64,
    })
}

/// Strip EXIF (including GPS) from JPEG, PNG, and WebP without re-encoding.
/// Falls back to returning the original bytes unchanged for unsupported formats.
fn strip_exif(data: &[u8], content_type: &str) -> Vec<u8> {
    let bytes: bytes::Bytes = data.to_vec().into();
    match content_type {
        ct if ct.contains("jpeg") || ct.contains("jpg") => {
            if let Ok(mut jpeg) = img_parts::jpeg::Jpeg::from_bytes(bytes) {
                jpeg.set_exif(None);
                jpeg.encoder().bytes().to_vec()
            } else {
                data.to_vec()
            }
        }
        ct if ct.contains("png") => {
            if let Ok(mut png) = img_parts::png::Png::from_bytes(bytes) {
                png.set_exif(None);
                png.encoder().bytes().to_vec()
            } else {
                data.to_vec()
            }
        }
        ct if ct.contains("webp") => {
            if let Ok(mut webp) = img_parts::webp::WebP::from_bytes(bytes) {
                webp.set_exif(None);
                webp.encoder().bytes().to_vec()
            } else {
                data.to_vec()
            }
        }
        _ => data.to_vec(),
    }
}

// ── GET /api/v1/media/:id ─────────────────────────────────────────────────

pub async fn get_media(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<MediaAttachment>> {
    auth.require_scope("write:media")?;
    let attachment = sqlx::query_as!(
        crate::db::models::MediaAttachment,
        "SELECT * FROM media_attachments WHERE id = $1 AND account_id = $2 AND status_id IS NULL",
        id, auth.account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(media_from_db(&attachment)))
}

// ── PUT /api/v1/media/:id ─────────────────────────────────────────────────

pub async fn update_media(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    request: axum::extract::Request,
) -> AppResult<Json<MediaAttachment>> {
    auth.require_scope("write:media")?;
    // Verify ownership before touching the body so a non-owner gets 404 even
    // when the body format is unexpected.
    sqlx::query!(
        "SELECT id FROM media_attachments WHERE id = $1 AND account_id = $2 AND status_id IS NULL",
        id, auth.account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let mut description: Option<String> = None;
    let mut focus: Option<String> = None;

    // Mastodon accepts both multipart/form-data and JSON bodies here.
    let is_multipart = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("multipart/form-data"));

    if is_multipart {
        use axum::extract::FromRequest;
        let mut multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|e| AppError::Unprocessable(e.to_string()))?;
        while let Some(field) = multipart.next_field().await.map_err(|e| AppError::Unprocessable(e.to_string()))? {
            let name = field.name().unwrap_or("").to_string();
            match name.as_str() {
                "description" => {
                    description = Some(field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?);
                }
                "focus" => {
                    focus = Some(field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?);
                }
                _ => {}
            }
        }
    } else {
        let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
            .await
            .map_err(|e| AppError::Unprocessable(e.to_string()))?;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({}));
        description = body.get("description").and_then(|v| v.as_str()).map(str::to_owned);
        focus = body.get("focus").and_then(|v| v.as_str()).map(str::to_owned);
    }

    if let Some(ref desc) = description {
        sqlx::query!(
            "UPDATE media_attachments SET description = $1 WHERE id = $2",
            desc, id,
        )
        .execute(&state.db)
        .await?;
    }

    if let Some(ref focus_str) = focus {
        // Parse "x,y" format into { focus: { x, y } } and merge into meta
        if let Some((x_str, y_str)) = focus_str.split_once(',') {
            if let (Ok(x), Ok(y)) = (x_str.trim().parse::<f64>(), y_str.trim().parse::<f64>()) {
                let current = sqlx::query_scalar!(
                    "SELECT file_meta FROM media_attachments WHERE id = $1",
                    id,
                )
                .fetch_one(&state.db)
                .await?;
                let mut meta = current.unwrap_or(serde_json::json!({}));
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("focus".to_string(), serde_json::json!({ "x": x, "y": y }));
                }
                sqlx::query!(
                    "UPDATE media_attachments SET file_meta = $1 WHERE id = $2",
                    meta, id,
                )
                .execute(&state.db)
                .await?;
            }
        }
    }

    let updated = sqlx::query_as!(
        crate::db::models::MediaAttachment,
        "SELECT * FROM media_attachments WHERE id = $1",
        id,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(Json(media_from_db(&updated)))
}

// ── DELETE /api/v1/media/:id ──────────────────────────────────────────────

pub async fn delete_media(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<axum::http::StatusCode> {
    auth.require_scope("write:media")?;
    let attachment = sqlx::query_as!(
        crate::db::models::MediaAttachment,
        "SELECT * FROM media_attachments WHERE id = $1 AND account_id = $2",
        id, auth.account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    if attachment.status_id.is_some() {
        return Err(AppError::Unprocessable("Media attachment is currently used by a status".into()));
    }

    sqlx::query!(
        "DELETE FROM media_attachments WHERE id = $1",
        id,
    )
    .execute(&state.db)
    .await?;

    // Delete file from S3 using computed key from file_file_name
    if let Some(filename) = &attachment.file_file_name {
        if !filename.is_empty() {
            let key = format!("media_attachments/files/{}/original/{}", crate::media::int_to_path(attachment.id), filename);
            let _ = state.storage.delete(&key).await;
        }
    }

    Ok(axum::http::StatusCode::OK)
}

fn classify_media_type(content_type: &str) -> &'static str {
    if content_type.starts_with("image/gif") {
        "gifv"
    } else if content_type.starts_with("image/") {
        "image"
    } else if content_type.starts_with("video/") {
        "video"
    } else if content_type.starts_with("audio/") {
        "audio"
    } else {
        "unknown"
    }
}

fn media_type_int(mt: &str) -> i32 {
    match mt { "image" => 0, "gifv" => 1, "video" => 2, "audio" => 3, _ => 4 }
}

pub fn media_type_str(type_int: Option<i32>) -> &'static str {
    match type_int {
        Some(0) => "image",
        Some(1) => "gifv",
        Some(2) => "video",
        Some(3) => "audio",
        _ => "unknown",
    }
}
