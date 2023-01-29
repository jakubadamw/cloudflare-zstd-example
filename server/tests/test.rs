#![feature(duration_constants)]

const INPUT: &str = include_str!("./input.txt");

#[derive(Debug, strum_macros::Display, PartialEq)]
#[strum(serialize_all = "lowercase")]
enum Encoding {
    Identity,
    Zstd,
}

async fn test_get(
    client: &reqwest::Client,
    key_url: &url::Url,
    accept_encoding: Encoding,
) -> anyhow::Result<Encoding> {
    let response = client
        .get(key_url.clone())
        .header("accept-encoding", accept_encoding.to_string())
        .send()
        .await?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok());
    assert_eq!(content_type, Some("text/plain"));
    let content_encoding = response
        .headers()
        .get("content-encoding")
        .and_then(|value| value.to_str().ok());
    match content_encoding {
        Some("zstd") => {
            let response_bytes = response.bytes().await?.to_vec();
            let decoded = zstd::stream::decode_all(response_bytes.as_slice())?;
            let decoded_string = String::from_utf8(decoded)?;
            assert_eq!(decoded_string, INPUT);
            Ok(Encoding::Zstd)
        }
        None => {
            assert_eq!(response.text().await?, INPUT);
            Ok(Encoding::Identity)
        }
        Some(content_encoding) => {
            anyhow::bail!("Unknown content encoding: {content_encoding}");
        }
    }
}

#[tokio::test]
async fn test_server() -> anyhow::Result<()> {
    const WAIT_UNTIL_ZSTD_IS_SERVED_ATTEMPT_COUNT: usize = 30;
    const WAIT_UNTIL_ZSTD_IS_SERVED_SLEEP_DURATION: std::time::Duration =
        std::time::Duration::SECOND;

    let domain = std::env::var("SERVER_DOMAIN_TO_TEST")?;
    let random_id = uuid::Uuid::new_v4().to_string();
    let mut key_url: url::Url = format!("https://{domain}").parse()?;
    key_url.path_segments_mut().unwrap().push(&random_id);

    let client = reqwest::Client::new();
    let response = client
        .post(key_url.clone())
        .header("content-type", "text/plain")
        .body(INPUT)
        .send()
        .await?;
    assert_eq!(
        response.text().await?,
        format!("{random_id} uploaded, compression scheduled")
    );

    // Don't accept zstd.
    assert_eq!(
        test_get(&client, &key_url, Encoding::Identity).await?,
        Encoding::Identity
    );

    for _ in 0..WAIT_UNTIL_ZSTD_IS_SERVED_ATTEMPT_COUNT {
        // Accept zstd.
        let encoding = test_get(&client, &key_url, Encoding::Zstd).await?;
        if encoding == Encoding::Zstd {
            return Ok(());
        }
        tokio::time::sleep(WAIT_UNTIL_ZSTD_IS_SERVED_SLEEP_DURATION).await;
    }

    anyhow::bail!("zstd-compressed payload has not become available by now")
}
