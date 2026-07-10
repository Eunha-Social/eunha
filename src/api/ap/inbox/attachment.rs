//! Pure helpers for parsing ActivityPub attachment/media JSON into the shapes
//! Eunha stores and serves. No I/O or database access — every function here is
//! a pure transformation of `serde_json` values.

use serde_json::Value;

/// Extract a usable `(href, media_type)` from an ActivityPub `url` value, which
/// may be a bare string, a `Link` object, or an array of either. When given an
/// array, prefers the first image/video/audio link, falling back to the first
/// link of any type.
pub(super) fn attachment_url(value: &Value) -> Option<(String, Option<String>)> {
    match value {
        Value::String(s) if !s.is_empty() => Some((s.clone(), None)),
        Value::Object(o) => {
            let href = o
                .get("href")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let media_type = o
                .get("mediaType")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Some((href.to_string(), media_type))
        }
        Value::Array(arr) => {
            let mut fallback: Option<(String, Option<String>)> = None;
            for el in arr {
                if let Some((href, media_type)) = attachment_url(el) {
                    let is_media = media_type.as_deref().is_some_and(|m| {
                        m.starts_with("image/")
                            || m.starts_with("video/")
                            || m.starts_with("audio/")
                    });
                    if is_media {
                        return Some((href, media_type));
                    }
                    if fallback.is_none() {
                        fallback = Some((href, media_type));
                    }
                }
            }
            fallback
        }
        _ => None,
    }
}

/// Build a Mastodon-style `file_meta` (`{"original": {...}, "focus": {...}}`)
/// from an ActivityPub attachment's `width`/`height`/`duration`/`focalPoint`.
/// Returns `None` when the attachment carries no geometry.
///
/// This must run on every media-ingestion path: the official iOS client sizes
/// its image grid by dividing the container width by the sum of the images'
/// aspect ratios, and an image with no dimensions contributes nothing — so a
/// post whose images all lack `meta.original.{width,height}` divides by zero,
/// producing a NaN layout that aborts the app (`CALayer position contains NaN`).
pub(super) fn ap_attachment_file_meta(att: &serde_json::Value) -> Option<serde_json::Value> {
    let width = att.get("width").and_then(|v| v.as_i64());
    let height = att.get("height").and_then(|v| v.as_i64());
    // Mastodon serializes a video's `duration` as an ISO8601 duration string
    // (e.g. "PT25.4S"); Misskey/others may send a number. `size`/`aspect` are
    // added at serialization (image only, mirroring Mastodon's meta shape).
    let duration = att.get("duration").and_then(parse_ap_duration);
    // focalPoint [x, y] -> meta.focus { x, y } (Mastodon's focus).
    let focus = att
        .get("focalPoint")
        .and_then(|v| v.as_array())
        .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
    if width.is_none() && height.is_none() && duration.is_none() && focus.is_none() {
        return None;
    }
    let mut meta = serde_json::Map::new();
    if width.is_some() || height.is_some() || duration.is_some() {
        let mut orig = serde_json::Map::new();
        if let Some(w) = width {
            orig.insert("width".into(), w.into());
        }
        if let Some(h) = height {
            orig.insert("height".into(), h.into());
        }
        if let Some(d) = duration {
            orig.insert("duration".into(), d.into());
        }
        meta.insert("original".into(), serde_json::Value::Object(orig));
    }
    if let Some((x, y)) = focus {
        meta.insert("focus".into(), serde_json::json!({ "x": x, "y": y }));
    }
    Some(serde_json::Value::Object(meta))
}

/// Parse an ActivityPub attachment `duration` into seconds. Accepts a plain
/// number or an ISO8601 duration's time part (`PT[nH][nM][nS]`, e.g. "PT25.4S").
pub(super) fn parse_ap_duration(v: &serde_json::Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let rest = v.as_str()?.strip_prefix("PT")?;
    let mut total = 0.0;
    let mut num = String::new();
    for c in rest.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let val: f64 = num.parse().ok()?;
            total += match c {
                'H' => val * 3600.0,
                'M' => val * 60.0,
                'S' => val,
                _ => return None,
            };
            num.clear();
        }
    }
    (total > 0.0).then_some(total)
}

/// Map an attachment's `type`/`mediaType` to Eunha's media-attachment type code
/// (0 image, 1 gifv, 2 video, 3 audio, 4 unknown).
pub(super) fn classify_attachment_type(att_type_str: &str, media_type_str: &str) -> i32 {
    if media_type_str == "image/gif" {
        1
    } else if media_type_str.starts_with("image/") {
        0
    } else if media_type_str.starts_with("video/") {
        2
    } else if media_type_str.starts_with("audio/") {
        3
    } else {
        match att_type_str {
            "Image" => 0,
            "Video" => {
                if media_type_str.contains("gif") {
                    1
                } else {
                    2
                }
            }
            "Audio" => 3,
            _ => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ap_duration_handles_iso8601_and_numbers() {
        use serde_json::json;
        // Mastodon sends ISO8601 time durations.
        assert_eq!(parse_ap_duration(&json!("PT25.4S")), Some(25.4));
        assert_eq!(parse_ap_duration(&json!("PT1M5S")), Some(65.0));
        assert_eq!(parse_ap_duration(&json!("PT1H2M3S")), Some(3723.0));
        // Misskey/others may send a plain number.
        assert_eq!(parse_ap_duration(&json!(12.5)), Some(12.5));
        // Garbage → None.
        assert_eq!(parse_ap_duration(&json!("nonsense")), None);
    }

    #[test]
    fn attachment_type_prefers_media_type_for_document_attachments() {
        assert_eq!(classify_attachment_type("Document", "image/webp"), 0);
        assert_eq!(classify_attachment_type("Document", "image/jpeg"), 0);
        assert_eq!(classify_attachment_type("Document", "image/gif"), 1);
        assert_eq!(classify_attachment_type("Document", "video/mp4"), 2);
        assert_eq!(classify_attachment_type("Document", "audio/mpeg"), 3);
    }

    #[test]
    fn attachment_type_falls_back_to_activitypub_type() {
        assert_eq!(classify_attachment_type("Image", ""), 0);
        assert_eq!(classify_attachment_type("Video", ""), 2);
        assert_eq!(classify_attachment_type("Audio", ""), 3);
        assert_eq!(classify_attachment_type("Document", ""), 4);
    }
}
