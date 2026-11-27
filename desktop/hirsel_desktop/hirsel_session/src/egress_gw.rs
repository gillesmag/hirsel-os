// Isolated Egress Gateway & Sealed Secret Vault Component (egress_gw.cm).
//
// Security boundary (Track 1 Hardening):
//   - Serves ONLY private Fuchsia FIDL `hirsel.egress.Gateway` (ZERO TCP listening ports!).
//   - Routed exclusively to `hirsel_shell.cm`; untrusted Chromium contexts and Linux MicroVMs
//     cannot connect to `egress_gw.cm` over loopback or any network interface!
//   - Owns its own isolated Fxfs `/data/secrets.json` directory and 99 Fuchsia root SSL certs
//     for real TLS 1.3 HTTPS via `rustls`.

use anyhow::{Context, Result};
use fidl_fuchsia_hirsel_egress::{GatewayRequest, GatewayRequestStream};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use log::{info, warn};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream as StdTcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};

const SECRETS_PATH: &str = "/data/secrets.json";
const CERT_PATH: &str = "/config/ssl/cert.pem";
const ALLOWED_HOSTS: &[&str] = &[
    "127.0.0.1",
    "10.0.2.2",
    "example.com",
    "api.github.com",
    "api.anthropic.com",
    "api.openai.com",
];

struct Vault {
    secrets: HashMap<String, String>,
    tls_config: Arc<rustls::ClientConfig>,
    tls_root_count: usize,
}

fn load_tls_config() -> (Arc<rustls::ClientConfig>, usize) {
    let mut root_store = rustls::RootCertStore::empty();
    let mut count = 0usize;
    if let Ok(f) = std::fs::File::open(CERT_PATH) {
        let mut reader = BufReader::new(f);
        for cert in rustls_pemfile::certs(&mut reader).flatten() {
            if root_store.add(cert).is_ok() {
                count += 1;
            }
        }
    } else {
        warn!("egress_gw: {CERT_PATH} not found; HTTPS verification will have 0 roots");
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    (Arc::new(config), count)
}

fn load_secrets() -> HashMap<String, String> {
    if let Ok(raw) = std::fs::read_to_string(SECRETS_PATH) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
            return map;
        }
    }
    let mut defaults = HashMap::new();
    defaults.insert("127.0.0.1".to_string(), "hirsel-host-sealed-demo-token-v1".to_string());
    defaults.insert("api.github.com".to_string(), "ghp_host_sealed_placeholder".to_string());
    save_secrets(&defaults);
    defaults
}

fn save_secrets(map: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if let Ok(mut f) = std::fs::File::create(SECRETS_PATH) {
            let _ = f.write_all(json.as_bytes()).and_then(|_| f.sync_all());
        }
    }
}

fn secret_header_for(host: &str, secret: &str) -> String {
    match host {
        "api.anthropic.com" => format!(
            "x-api-key: {secret}\r\nanthropic-version: 2023-06-01\r\nContent-Type: application/json\r\n"
        ),
        "api.openai.com" => {
            format!("Authorization: Bearer {secret}\r\nContent-Type: application/json\r\n")
        }
        "api.github.com" => format!(
            "Authorization: Bearer {secret}\r\nAccept: application/vnd.github+json\r\n"
        ),
        _ => format!("X-Hirsel-Egress-Key: {secret}\r\n"),
    }
}

fn do_outbound_request(
    host: &str,
    port: u16,
    use_tls: bool,
    method: &str,
    path: &str,
    body: &str,
    secret: Option<&str>,
    tls_config: Arc<rustls::ClientConfig>,
) -> Result<(u16, String, bool)> {
    let addr = if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, port)
    } else {
        let mut iter = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("DNS resolve {host}:{port}"))?;
        iter.next().ok_or_else(|| anyhow::anyhow!("no IP resolved for {host}:{port}"))?
    };

    let safe_path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let host_hdr = if (use_tls && port == 443) || (!use_tls && port == 80) {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let secret_hdr = secret.map(|s| secret_header_for(host, s)).unwrap_or_default();
    let req = format!(
        "{method} {safe_path} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: Hirsel-OS-EgressGW/0.3-FIDL\r\n{secret_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let mut tcp = StdTcpStream::connect_timeout(&addr, std::time::Duration::from_secs(10))
        .with_context(|| format!("TCP connect {addr}"))?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(10)));

    let mut resp = Vec::with_capacity(4096);
    if use_tls {
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|_| anyhow::anyhow!("invalid TLS server name: {host}"))?;
        let conn = rustls::ClientConnection::new(tls_config, server_name)
            .context("rustls ClientConnection::new")?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        tls.write_all(req.as_bytes()).context("TLS write HTTP request")?;
        tls.flush().ok();
        let mut buf = [0u8; 2048];
        loop {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    resp.extend_from_slice(&buf[..n]);
                    if resp.len() >= 49152 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    } else {
        tcp.write_all(req.as_bytes()).context("TCP write HTTP request")?;
        tcp.flush().ok();
        let mut buf = [0u8; 2048];
        loop {
            match tcp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    resp.extend_from_slice(&buf[..n]);
                    if resp.len() >= 49152 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }

    let text = String::from_utf8_lossy(&resp).to_string();
    let status_code = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let resp_body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
    Ok((status_code, resp_body, use_tls))
}

async fn handle_gateway_stream(mut stream: GatewayRequestStream, vault: Arc<Mutex<Vault>>) {
    while let Ok(Some(req)) = stream.try_next().await {
        match req {
            GatewayRequest::Health { responder } => {
                let (roots, host_count) = vault
                    .lock()
                    .map(|v| (v.tls_root_count, v.secrets.len()))
                    .unwrap_or((0, 0));
                let payload = serde_json::json!({
                    "ok": true,
                    "component": "egress_gw.cm",
                    "transport": "fidl_only_zero_tcp",
                    "tls_roots": roots,
                    "sealed_hosts": host_count,
                    "isolated_vault": true
                })
                .to_string();
                let _ = responder.send(&payload);
            }
            GatewayRequest::ListSecrets { responder } => {
                let meta: Vec<serde_json::Value> = vault
                    .lock()
                    .map(|v| {
                        let mut list: Vec<_> = v
                            .secrets
                            .iter()
                            .map(|(h, val)| {
                                let is_placeholder = val.contains("placeholder") || val.contains("demo-token");
                                serde_json::json!({
                                    "host": h,
                                    "len": val.len(),
                                    "sealed": true,
                                    "live": !is_placeholder
                                })
                            })
                            .collect();
                        list.sort_by(|a, b| a["host"].as_str().cmp(&b["host"].as_str()));
                        list
                    })
                    .unwrap_or_default();
                let payload = serde_json::json!({"secrets": meta, "write_only": true}).to_string();
                let _ = responder.send(&payload);
            }
            GatewayRequest::StoreSecret { host, value, sas_authorized, responder } => {
                if !sas_authorized {
                    let payload = serde_json::json!({
                        "stored": false,
                        "reason": "sas_trusted_chrome_required"
                    })
                    .to_string();
                    let _ = responder.send(403, &payload);
                    continue;
                }
                let host = host.trim().to_string();
                if host.is_empty() || value.is_empty() {
                    let _ = responder.send(400, r#"{"stored":false,"reason":"missing_host_or_value"}"#);
                    continue;
                }
                let count = if let Ok(mut v) = vault.lock() {
                    v.secrets.insert(host.clone(), value);
                    save_secrets(&v.secrets);
                    v.secrets.len()
                } else {
                    0
                };
                let payload = serde_json::json!({
                    "stored": true,
                    "host": host,
                    "sealed_hosts": count,
                    "transport": "fidl"
                })
                .to_string();
                let _ = responder.send(200, &payload);
            }
            GatewayRequest::Proxy_ { request_json, responder } => {
                let parsed = serde_json::from_str::<serde_json::Value>(&request_json).unwrap_or_default();
                let host = parsed["host"].as_str().unwrap_or("").to_string();
                let scheme = parsed["scheme"].as_str().unwrap_or("http").to_string();
                let use_tls = scheme.eq_ignore_ascii_case("https");
                let default_port = if use_tls { 443 } else { 80 };
                let port = parsed["port"].as_u64().unwrap_or(default_port) as u16;
                let p_method = parsed["method"].as_str().unwrap_or("GET").to_string();
                let p_path = parsed["path"].as_str().unwrap_or("/").to_string();
                let p_body = parsed["body"].as_str().unwrap_or("").to_string();

                if !ALLOWED_HOSTS.contains(&host.as_str()) {
                    let payload = serde_json::json!({
                        "allowed": false,
                        "reason": "host_not_in_capability_allowlist",
                        "gateway": "egress_gw.cm",
                        "transport": "fidl",
                        "host": host
                    })
                    .to_string();
                    let _ = responder.send(403, &payload);
                    continue;
                }

                let (secret, tls_cfg) = vault
                    .lock()
                    .map(|v| (v.secrets.get(&host).cloned(), Arc::clone(&v.tls_config)))
                    .unwrap();
                let had_secret = secret.is_some();
                let host_c = host.clone();
                let res = fasync::unblock(move || {
                    do_outbound_request(
                        &host_c,
                        port,
                        use_tls,
                        &p_method,
                        &p_path,
                        &p_body,
                        secret.as_deref(),
                        tls_cfg,
                    )
                })
                .await;

                match res {
                    Ok((http_status, resp_body, tls_used)) => {
                        let payload = serde_json::json!({
                            "allowed": true,
                            "gateway": "egress_gw.cm",
                            "transport": "fidl",
                            "host": host,
                            "port": port,
                            "tls": tls_used,
                            "http_status": http_status,
                            "secret_injected": had_secret,
                            "body": resp_body
                        })
                        .to_string();
                        let _ = responder.send(200, &payload);
                    }
                    Err(e) => {
                        let payload = serde_json::json!({
                            "allowed": true,
                            "gateway": "egress_gw.cm",
                            "transport": "fidl",
                            "host": host,
                            "tls": use_tls,
                            "error": format!("{e:#}")
                        })
                        .to_string();
                        let _ = responder.send(502, &payload);
                    }
                }
            }
        }
    }
}

enum IncomingService {
    Gateway(GatewayRequestStream),
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<()> {
    let (tls_config, tls_root_count) = load_tls_config();
    let secrets = load_secrets();
    info!(
        "egress_gw.cm starting (FIDL-only hirsel.egress.Gateway, zero TCP ports, tls_roots={tls_root_count}, sealed_hosts={})",
        secrets.len()
    );
    let vault = Arc::new(Mutex::new(Vault { secrets, tls_config, tls_root_count }));

    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingService::Gateway);
    fs.take_and_serve_directory_handle().context("serve outgoing FIDL dir")?;

    fs.for_each_concurrent(None, |service| {
        let v = Arc::clone(&vault);
        async move {
            match service {
                IncomingService::Gateway(stream) => handle_gateway_stream(stream, v).await,
            }
        }
    })
    .await;
    Ok(())
}
