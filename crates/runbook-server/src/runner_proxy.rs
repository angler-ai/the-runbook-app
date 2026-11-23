//! R0-5 on the native server: the outbound client of the runner proxy
//! (`runbook_api::runner_proxy`). The runner's address is resolved and
//! vetted on every request under the webhook target rules (private
//! addresses only with `RUNBOOK_RUNNER_PRIVATE_NETWORKS=true`, never
//! link-local or a metadata host), the connection goes only to the vetted
//! addresses, redirects are never followed, the body is bounded and the
//! call times out. With `RUNBOOK_RUNNER_CLIENT_CERT` the server presents a
//! client certificate (mutual TLS). Nothing here logs a header or a body.
use crate::{config::RunnerConfig, webhooks::system_resolver};
use runbook_api::runner_proxy::{
    MAX_RESPONSE_BYTES, RunnerHttpRequest, RunnerHttpResponse, TIMEOUT_MS,
};
use runbook_core::webhooks::TargetPolicy;
use std::{io::Read, sync::Arc, time::Duration};

/// A rustls client configuration presenting the PEM file's certificate
/// chain and private key, trusting the public web roots.
pub fn client_tls(pem: &[u8]) -> Result<Arc<rustls::ClientConfig>, &'static str> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<_, _>>()
        .map_err(|_| "config.invalid_runner_client_cert")?;
    if certs.is_empty() {
        return Err("config.invalid_runner_client_cert");
    }
    let key =
        PrivateKeyDer::from_pem_slice(pem).map_err(|_| "config.invalid_runner_client_cert")?;
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "config.invalid_runner_client_cert")?
    .with_root_certificates(roots)
    .with_client_auth_cert(certs, key)
    .map_err(|_| "config.invalid_runner_client_cert")?;
    Ok(Arc::new(config))
}

/// One blocking request to the runner (run on the blocking pool).
pub fn fetch(
    config: &RunnerConfig,
    request: RunnerHttpRequest,
) -> Result<RunnerHttpResponse, &'static str> {
    let base = request.url.split('?').next().unwrap_or_default();
    let allow_private = config.private_networks
        && config.private_url.as_ref().is_none_or(|pin| {
            base == pin
                || base
                    .strip_prefix(pin)
                    .is_some_and(|rest| rest.starts_with('/'))
        });
    runbook_core::runners::validate_url(base, allow_private).map_err(|_| "runner.url_forbidden")?;
    let policy = TargetPolicy {
        allow_private,
        allow_link_local: false,
    };
    let resolve = config.resolver.clone().unwrap_or_else(system_resolver);
    let addrs = crate::webhooks::vet(&request.url, policy, &resolve).map_err(|e| match e {
        "webhooks.target_unresolved" => "runner.unreachable",
        _ => "runner.url_forbidden",
    })?;
    let timeout = Duration::from_millis(TIMEOUT_MS);
    let mut agent = ureq::AgentBuilder::new()
        .resolver(move |_: &str| Ok(addrs.clone()))
        .redirects(0)
        .timeout(timeout);
    if let Some(pem) = &config.client_cert_pem {
        agent = agent.tls_config(client_tls(pem)?);
    }
    let mut call = agent.build().request(&request.method, &request.url);
    for (name, value) in &request.headers {
        call = call.set(name, value);
    }
    let result = if request.body.is_empty() {
        call.call()
    } else {
        call.send_bytes(&request.body)
    };
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(transport)) => {
            return Err(match transport.kind() {
                ureq::ErrorKind::Io => "runner.timeout",
                _ => "runner.unreachable",
            });
        }
    };
    let status = response.status();
    if (300..400).contains(&status) {
        return Err("runner.bad_response");
    }
    let content_type = response.header("content-type").map(str::to_owned);
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| "runner.unreachable")?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("runner.bad_response");
    }
    Ok(RunnerHttpResponse {
        status,
        content_type,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_runner_proxy_allows_only_the_pinned_service() {
        let config = RunnerConfig {
            private_networks: true,
            private_url: Some("http://runner-api.runbook-runner.svc.cluster.local:8090".to_owned()),
            resolver: Some(Arc::new(|host, port| {
                assert_eq!(host, "runner-api.runbook-runner.svc.cluster.local");
                assert_eq!(port, 8090);
                Ok(Vec::new())
            })),
            ..RunnerConfig::default()
        };
        let send = |url: &str| {
            fetch(
                &config,
                RunnerHttpRequest {
                    url: url.to_owned(),
                    method: "GET".to_owned(),
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            )
            .err()
        };
        assert_eq!(
            send("http://runner-api.runbook-runner.svc.cluster.local:8090/runner/v1/status"),
            Some("runner.unreachable")
        );
        for other in [
            "http://runner-api.runbook-runner.svc.cluster.local:8091/runner/v1/status",
            "http://runner-api.runbook-runner.svc.cluster.local:80900/runner/v1/status",
            "http://127.0.0.1:8090/runner/v1/status",
            "https://10.0.0.4:8090/runner/v1/status",
        ] {
            assert_eq!(send(other), Some("runner.url_forbidden"));
        }
    }
}
