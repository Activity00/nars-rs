//! Shared HTTP plumbing for provider adapters.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::provider::ModelError;

/// Sends a JSON POST and returns the status and body text.
pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
    timeout_ms: u64,
) -> Result<(u16, String), ModelError> {
    let mut header_map = HeaderMap::new();
    header_map.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|error| ModelError::provider(format!("invalid header `{key}`: {error}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| ModelError::provider(format!("invalid header value: {error}")))?;
        header_map.insert(name, value);
    }

    let request = client
        .post(url)
        .headers(header_map)
        .timeout(Duration::from_millis(timeout_ms.max(1)))
        .json(body)
        .send();
    let response = request.await.map_err(|error| {
        if error.is_timeout() {
            ModelError::retryable(format!("provider request timed out: {error}"))
        } else if error.is_connect() {
            ModelError::retryable(format!("provider connection failed: {error}"))
        } else {
            ModelError::provider(format!("provider request failed: {error}"))
        }
    })?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|error| {
        ModelError::retryable(format!("failed to read provider response: {error}"))
    })?;
    if !(200..300).contains(&status) {
        return Err(ModelError::http(status, text));
    }
    Ok((status, text))
}

/// Builds a plain `Bearer` authorization header when a key exists.
pub(crate) fn bearer(api_key: Option<&str>) -> Vec<(String, String)> {
    api_key
        .filter(|key| !key.is_empty())
        .map(|key| vec![("authorization".to_owned(), format!("Bearer {key}"))])
        .unwrap_or_default()
}

/// Trims trailing slashes from a base URL.
pub(crate) fn normalize_base(base: &str) -> String {
    base.trim_end_matches('/').to_owned()
}
