#![allow(deprecated)]

use common::{full_key, KeyType, TryCoalesceExt};

use worker::*;

#[event(start)]
pub fn main() {
    console_error_panic_hook::set_once();
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    use futures_util::{StreamExt, TryStreamExt};

    let router = Router::new();

    router
        .post_async("/:key", |mut req, ctx| async move {
            let r2_bucket = ctx.bucket("BUCKET")?;
            let queue = ctx.env.queue("QUEUE")?;

            let key = ctx.param("key").unwrap();

            console_log!("Persisting {key}");

            let content_type = req
                .headers()
                .get("content-type")?
                .unwrap_or_else(|| mime::APPLICATION_OCTET_STREAM.to_string());
            let multipart_upload = r2_bucket
                .create_multipart_upload(full_key(key, KeyType::Raw))
                .http_metadata(HttpMetadata {
                    content_type: Some(content_type),
                    ..Default::default()
                })
                .execute()
                .await?;

            let multipart_parts = req.stream()?.try_coalesce(|mut acc, mut item| async {
                Ok(if acc.len() < common::R2_MULTIPART_CHUNK_MIN_SIZE {
                    acc.append(&mut item);
                    Ok(acc)
                } else {
                    Err((acc, item))
                })
            });
            let uploaded_parts: Vec<_> = multipart_parts
                .enumerate()
                .map(|(index, result)| result.map(|chunk| (index, chunk)))
                .and_then(|(index, chunk)| multipart_upload.upload_part(index as u16 + 1, chunk))
                .try_collect()
                .await?;
            multipart_upload
                .complete(uploaded_parts.into_iter())
                .await?;

            console_log!("Scheduling compression of {key}");

            queue.send(key).await?;
            Response::ok(format!("{key} uploaded, compression scheduled"))
        })
        .get_async("/:key", |req, ctx| async move {
            let r2_bucket = ctx.bucket("BUCKET")?;

            let key = ctx.param("key").unwrap();

            let accept_encoding_value = req.headers().get("accept-encoding")?;

            let client_accepts_zstd = accept_encoding_value
                .iter()
                .flat_map(|value| value.split(','))
                .map(str::trim)
                .map(|value| value.split(';').next().unwrap())
                .any(|encoding_name| encoding_name == "zstd");

            console_log!("Retrieving {key} (zstd accepted by the client: {client_accepts_zstd}), header value: {accept_encoding_value:?}");

            let compressed_object = if client_accepts_zstd {
                r2_bucket
                    .get(full_key(key, KeyType::CompressedZstd))
                    .execute()
                    .await?
            } else {
                None
            };

            if let Some(compressed_object) = compressed_object {
                console_log!("Serving compressed object");

                let mut headers = Headers::new();
                headers.set("content-encoding", "zstd")?;
                headers.set("content-length", &compressed_object.size().to_string())?;
                headers.set(
                    "content-type",
                    &compressed_object
                        .http_metadata()
                        .content_type
                        .unwrap_or_else(|| mime::APPLICATION_OCTET_STREAM.to_string()),
                )?;

                Ok(
                    Response::from_stream(compressed_object.body().unwrap().stream()?)?
                        .with_headers(headers),
                )
            } else if let Some(uncompressed_object) =
                r2_bucket.get(full_key(key, KeyType::Raw)).execute().await?
            {
                console_log!("Serving raw object");

                let mut headers = Headers::new();
                headers.set("content-length", &uncompressed_object.size().to_string())?;
                headers.set(
                    "content-type",
                    &uncompressed_object
                        .http_metadata()
                        .content_type
                        .unwrap_or_else(|| mime::APPLICATION_OCTET_STREAM.to_string()),
                )?;

                Ok(
                    Response::from_stream(uncompressed_object.body().unwrap().stream()?)?
                        .with_headers(headers),
                )
            } else {
                Response::error(format!("{key} not found"), 404)
            }
        })
        .run(req, env)
        .await
}
