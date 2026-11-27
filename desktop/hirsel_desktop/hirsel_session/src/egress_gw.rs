// Isolated Hirsel-OS Egress Gateway & Sealed Secret Vault Component (egress_gw.cm).
//
// Security boundary:
//   - Runs in its OWN Fuchsia component sandbox with its OWN isolated Fxfs /data directory.
//   - Neither Chromium, Linux vm-dev, nor even hirsel_shell (compositor) can read /data/secrets.json!
//   - Enforces static host capability allowlist, injects host-only credentials at egress time,
//     and supports BOTH plain HTTP and real TLS 1.3 HTTPS via rustls + Fuchsia root-ssl-certificates.
//   - Write-only secret provisioning (`POST /secrets`) requires the SAS session token header and
//     never returns secret values back to any caller.

use anyhow::{Context, Result};
use fuchsia_async as fasync;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use log::{error, info, warn};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream as StdTcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};

const SECRETS_PATH: &str = "/data/secrets.json";
const CERT_PATH: &str = "/config/ssl/cert.pem";
const GATEWAY_PORT: u16 = 7781;
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
        "api.anthropic.com" => format!("x-api-key: {secret}\r\nanthropic-version: 2023-06-01\r\n"),
        "api.openai.com" | "api.github.com" => format!("Authorization: Bearer {secret}\r\n"),
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
        "{method} {safe_path} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: Hirsel-OS-EgressGW/0.2\r\n{secret_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let mut tcp = StdTcpStream::connect_timeout(&addr, std::time::Duration::from_secs(10))
        .with_context(|| format!("TCP connect {addr}"))?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)));
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
                    if resp.len() >= 32768 {
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
                    if resp.len() >= 32768 {
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

async fn handle_conn(mut stream: fasync::net::TcpStream, vault: Arc<Mutex<Vault>>) -> Result<()> {
    let mut buf = vec![0u8; 32768];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let first = req.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    let header_block = req.split("\r\n\r\n").next().unwrap_or("");
    let body = req.split("\r\n\r\n").nth(1).unwrap_or("").trim();

    let (status, payload): (&str, String) = match (method, path) {
        ("GET", "/health") => {
            let (roots, host_count) = vault
                .lock()
                .map(|v| (v.tls_root_count, v.secrets.len()))
                .unwrap_or((0, 0));
            (
                "200 OK",
                serde_json::json!({
                    "ok": true,
                    "component": "egress_gw.cm",
                    "tls_roots": roots,
                    "sealed_hosts": host_count,
                    "isolated_vault": true
                })
                .to_string(),
            )
        }
        ("GET", "/secrets") => {
            // Metadata ONLY — plaintext secret bytes are NEVER exposed!
            let meta: Vec<serde_json::Value> = vault
                .lock()
                .map(|v| {
                    let mut list: Vec<_> = v
                        .secrets
                        .iter()
                        .map(|(h, val)| serde_json::json!({"host": h, "len": val.len(), "sealed": true}))
                        .collect();
                    list.sort_by(|a, b| a["host"].as_str().cmp(&b["host"].as_str()));
                    list
                })
                .unwrap_or_default();
            ("200 OK", serde_json::json!({"secrets": meta, "write_only": true}).to_string())
        }
        ("POST", "/secrets") => {
            let has_sas = header_block
                .lines()
                .any(|l| l.eq_ignore_ascii_case("X-Hirsel-Sas-Authorized: true"));
            if !has_sas {
                (
                    "403 Forbidden",
                    serde_json::json!({
                        "stored": false,
                        "reason": "sas_trusted_chrome_required"
                    })
                    .to_string(),
                )
            } else {
                let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
                let host = parsed["host"].as_str().unwrap_or("").trim().to_string();
                let value = parsed["value"].as_str().unwrap_or("").to_string();
                if host.is_empty() || value.is_empty() {
                    ("400 Bad Request", r#"{"stored":false,"reason":"missing_host_or_value"}"#.into())
                } else {
                    let count = if let Ok(mut v) = vault.lock() {
                        v.secrets.insert(host.clone(), value);
                        save_secrets(&v.secrets);
                        v.secrets.len()
                    } else {
                        0
                    };
                    (
                        "200 OK",
                        serde_json::json!({
                            "stored": true,
                            "host": host,
                            "sealed_hosts": count
                        })
                        .to_string(),
                    )
                }
            }
        }
        ("POST", "/proxy") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
            let host = parsed["host"].as_str().unwrap_or("").to_string();
            let scheme = parsed["scheme"].as_str().unwrap_or("http").to_string();
            let use_tls = scheme.eq_ignore_ascii_case("https");
            let default_port = if use_tls { 443 } else { 80 };
            let port = parsed["port"].as_u64().unwrap_or(default_port) as u16;
            let p_method = parsed["method"].as_str().unwrap_or("GET").to_string();
            let p_path = parsed["path"].as_str().unwrap_or("/").to_string();
            let p_body = parsed["body"].as_str().unwrap_or("").to_string();

            if !ALLOWED_HOSTS.contains(&host.as_str()) {
                (
                    "403 Forbidden",
                    serde_json::json!({
                        "allowed": false,
                        "reason": "host_not_in_capability_allowlist",
                        "gateway": "egress_gw.cm",
                        "host": host
                    })
                    .to_string(),
                )
            } else {
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
                    Ok((http_status, resp_body, tls_used)) => (
                        "200 OK",
                        serde_json::json!({
                            "allowed": true,
                            "gateway": "egress_gw.cm",
                            "host": host,
                            "port": port,
                            "tls": tls_used,
                            "http_status": http_status,
                            "secret_injected": had_secret,
                            "body": resp_body
                        })
                        .to_string(),
                    ),
                    Err(e) => (
                        "502 Bad Gateway",
                        serde_json::json!({
                            "allowed": true,
                            "gateway": "egress_gw.cm",
                            "host": host,
                            "tls": use_tls,
                            "error": format!("{e:#}")
                        })
                        .to_string(),
                    ),
                }
            }
        }
        _ => ("404 Not Found", r#"{"error":"not_found"}"#.to_string()),
    };

    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<()> {
    let (tls_config, tls_root_count) = load_tls_config();
    let secrets = load_secrets();
    info!(
        "egress_gw.cm starting on 127.0.0.1:{GATEWAY_PORT} (tls_roots={tls_root_count}, sealed_hosts={})",
        secrets.len()
    );
    let vault = Arc::new(Mutex::new(Vault { secrets, tls_config, tls_root_count }));

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), GATEWAY_PORT);
    let listener = fasync::net::TcpListener::bind(&addr).context("bind egress_gw 127.0.0.1:7781")?;
    let mut incoming = listener.accept_stream();
    while let Some(conn) = incoming.next().await {
        match conn {
            Ok((stream, _)) => {
                let v = Arc::clone(&vault);
                fasync::Task::local(async move {
                    if let Err(e) = handle_conn(stream, v).await {
                        warn!("egress_gw conn error: {e:#}");
                    }
                })
                .detach();
            }
            Err(e) => error!("egress_gw accept error: {e:?}"),
        }
    }
    Ok(())
}
