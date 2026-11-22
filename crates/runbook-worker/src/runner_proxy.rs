//! Bounded outbound transport for the shared runner proxy.
use futures_util::{
    StreamExt,
    future::{Either, select},
};
use runbook_api::runner_proxy::{
    MAX_RESPONSE_BYTES, RunnerHttpRequest, RunnerHttpResponse, TIMEOUT_MS,
};
use runbook_core::{runners::validate_url, webhooks::TargetPolicy};
use std::time::Duration;
use worker::{
    AbortController, Delay, Fetch, Fetcher, Headers, Request, RequestInit, RequestRedirect,
};

pub async fn fetch(
    mtls: Option<&Fetcher>,
    request: RunnerHttpRequest,
) -> Result<RunnerHttpResponse, &'static str> {
    let base = request.url.split('?').next().unwrap_or_default();
    let policy = TargetPolicy {
        allow_private: false,
        allow_link_local: false,
    };
    validate_url(base, policy.allow_private).map_err(|_| "runner.url_forbidden")?;
    crate::notify::vet(&request.url, policy).map_err(|_| "runner.url_forbidden")?;

    let headers = Headers::new();
    for (name, value) in request.headers {
        headers
            .set(&name, &value)
            .map_err(|_| "runner.unreachable")?;
    }
    let mut init = RequestInit::new();
    init.with_headers(headers)
        .with_method(request.method.into())
        .with_redirect(RequestRedirect::Manual);
    if !request.body.is_empty() {
        init.with_body(Some(
            js_sys::Uint8Array::from(request.body.as_slice()).into(),
        ));
    }

    let controller = AbortController::default();
    let signal = controller.signal();
    let init = worker::web_sys::RequestInit::from(&init);
    init.set_signal(Some(signal.as_ref()));
    let outgoing = worker::web_sys::Request::new_with_str_and_init(&request.url, &init)
        .map(Request::from)
        .map_err(|_| "runner.url_forbidden")?;
    let exchange = async {
        let mut response = match mtls {
            Some(binding) => binding.fetch_request(outgoing).await,
            None => Fetch::Request(outgoing).send_with_signal(&signal).await,
        }
        .map_err(|_| "runner.unreachable")?;
        let status = response.status_code();
        if (300..400).contains(&status) {
            return Err("runner.bad_response");
        }
        let content_type = response
            .headers()
            .get("content-type")
            .map_err(|_| "runner.bad_response")?;
        let mut body = Vec::new();
        if status != 204 && status != 205 {
            let mut stream = response.stream().map_err(|_| "runner.bad_response")?;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| "runner.unreachable")?;
                if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                    return Err("runner.bad_response");
                }
                body.extend_from_slice(&chunk);
            }
        }
        Ok(RunnerHttpResponse {
            status,
            content_type,
            body,
        })
    };
    match select(
        Box::pin(exchange),
        Box::pin(Delay::from(Duration::from_millis(TIMEOUT_MS))),
    )
    .await
    {
        Either::Left((result, _)) => {
            if result.is_err() {
                controller.abort();
            }
            result
        }
        Either::Right(((), _)) => {
            controller.abort();
            Err("runner.timeout")
        }
    }
}
