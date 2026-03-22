// Copyright 2026 Paul Adamson
// Licensed under the Apache License, Version 2.0
//
// APIRequestContext protocol object
//
// Enables performing HTTP requests without a browser, and is also used
// by Route.fetch() to perform the actual network request before modification.
//
// See: https://playwright.dev/docs/api/class-apirequestcontext

use crate::error::Result;
use crate::protocol::route::FetchResponse;
use crate::server::channel::Channel;
use crate::server::channel_owner::{
    ChannelOwner, ChannelOwnerImpl, DisposeReason, ParentOrConnection,
};
use crate::server::connection::ConnectionLike;
use serde_json::{Value, json};
use std::any::Any;
use std::sync::Arc;

/// APIRequestContext provides methods for making HTTP requests.
///
/// This is the Playwright protocol object that performs actual HTTP operations.
/// It is created automatically for each BrowserContext and can be accessed
/// via `BrowserContext::request()`.
///
/// Used internally by `Route::fetch()` to perform the actual network request.
///
/// See: <https://playwright.dev/docs/api/class-apirequestcontext>
#[derive(Clone)]
pub struct APIRequestContext {
    base: ChannelOwnerImpl,
}

impl APIRequestContext {
    pub fn new(
        parent: ParentOrConnection,
        type_name: String,
        guid: Arc<str>,
        initializer: Value,
    ) -> Result<Self> {
        Ok(Self {
            base: ChannelOwnerImpl::new(parent, type_name, guid, initializer),
        })
    }

    /// Performs an HTTP fetch request and returns the response.
    ///
    /// This is the internal method used by `Route::fetch()`. It sends the request
    /// via the Playwright server and returns the response with headers and body.
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to fetch
    /// * `options` - Optional parameters to customize the request
    ///
    /// See: <https://playwright.dev/docs/api/class-apirequestcontext#api-request-context-fetch>
    pub(crate) async fn inner_fetch(
        &self,
        url: &str,
        options: Option<InnerFetchOptions>,
    ) -> Result<FetchResponse> {
        let opts = options.unwrap_or_default();

        let mut params = json!({
            "url": url,
            "timeout": opts.timeout.unwrap_or(crate::DEFAULT_TIMEOUT_MS)
        });

        if let Some(method) = opts.method {
            params["method"] = json!(method);
        }
        if let Some(headers) = opts.headers {
            let headers_array: Vec<Value> = headers
                .into_iter()
                .map(|(name, value)| json!({"name": name, "value": value}))
                .collect();
            params["headers"] = json!(headers_array);
        }
        if let Some(post_data) = opts.post_data {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(post_data.as_bytes());
            params["postData"] = json!(encoded);
        }
        if let Some(post_data_bytes) = opts.post_data_bytes {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&post_data_bytes);
            params["postData"] = json!(encoded);
        }
        if let Some(max_redirects) = opts.max_redirects {
            params["maxRedirects"] = json!(max_redirects);
        }
        if let Some(max_retries) = opts.max_retries {
            params["maxRetries"] = json!(max_retries);
        }

        // Call the fetch command on APIRequestContext channel
        #[derive(serde::Deserialize)]
        struct FetchResult {
            response: ApiResponseData,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ApiResponseData {
            fetch_uid: String,
            #[allow(dead_code)]
            url: String,
            status: u16,
            status_text: String,
            headers: Vec<HeaderEntry>,
        }

        #[derive(serde::Deserialize)]
        struct HeaderEntry {
            name: String,
            value: String,
        }

        let result: FetchResult = self.base.channel().send("fetch", params).await?;

        // Now fetch the response body using fetchResponseBody
        let body = self.fetch_response_body(&result.response.fetch_uid).await?;

        // Dispose the API response to free server resources
        let _ = self.dispose_api_response(&result.response.fetch_uid).await;

        Ok(FetchResponse {
            status: result.response.status,
            status_text: result.response.status_text,
            headers: result
                .response
                .headers
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
            body,
        })
    }

    /// Fetches the response body for a given fetch UID.
    async fn fetch_response_body(&self, fetch_uid: &str) -> Result<Vec<u8>> {
        #[derive(serde::Deserialize)]
        struct BodyResult {
            #[serde(default)]
            binary: Option<String>,
        }

        let result: BodyResult = self
            .base
            .channel()
            .send("fetchResponseBody", json!({ "fetchUid": fetch_uid }))
            .await?;

        match result.binary {
            Some(encoded) if !encoded.is_empty() => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(&encoded)
                    .map_err(|e| {
                        crate::error::Error::ProtocolError(format!(
                            "Failed to decode response body: {}",
                            e
                        ))
                    })
            }
            _ => Ok(vec![]),
        }
    }

    /// Disposes an API response to free server resources.
    async fn dispose_api_response(&self, fetch_uid: &str) -> Result<()> {
        self.base
            .channel()
            .send_no_result("disposeAPIResponse", json!({ "fetchUid": fetch_uid }))
            .await
    }
}

/// Options for APIRequestContext.inner_fetch()
#[derive(Debug, Clone, Default)]
pub(crate) struct InnerFetchOptions {
    pub method: Option<String>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub post_data: Option<String>,
    pub post_data_bytes: Option<Vec<u8>>,
    pub max_redirects: Option<u32>,
    pub max_retries: Option<u32>,
    pub timeout: Option<f64>,
}

impl ChannelOwner for APIRequestContext {
    fn guid(&self) -> &str {
        self.base.guid()
    }

    fn type_name(&self) -> &str {
        self.base.type_name()
    }

    fn parent(&self) -> Option<Arc<dyn ChannelOwner>> {
        self.base.parent()
    }

    fn connection(&self) -> Arc<dyn ConnectionLike> {
        self.base.connection()
    }

    fn initializer(&self) -> &Value {
        self.base.initializer()
    }

    fn channel(&self) -> &Channel {
        self.base.channel()
    }

    fn dispose(&self, reason: DisposeReason) {
        self.base.dispose(reason)
    }

    fn adopt(&self, child: Arc<dyn ChannelOwner>) {
        self.base.adopt(child)
    }

    fn add_child(&self, guid: Arc<str>, child: Arc<dyn ChannelOwner>) {
        self.base.add_child(guid, child)
    }

    fn remove_child(&self, guid: &str) {
        self.base.remove_child(guid)
    }

    fn on_event(&self, method: &str, params: Value) {
        self.base.on_event(method, params)
    }

    fn was_collected(&self) -> bool {
        self.base.was_collected()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl std::fmt::Debug for APIRequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("APIRequestContext")
            .field("guid", &self.guid())
            .finish()
    }
}
