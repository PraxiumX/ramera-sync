use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::StatusCode;

pub async fn get_with_auth(
    client: &reqwest::Client,
    url: &str,
    username: Option<&str>,
    password: Option<&str>,
    timeout_ms: u64,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let timeout = Duration::from_millis(timeout_ms);
    let (Some(username), Some(password)) = (username, password) else {
        return client.get(url).timeout(timeout).send().await;
    };

    let initial = client
        .get(url)
        .timeout(timeout)
        .basic_auth(username, Some(password))
        .send()
        .await?;

    if initial.status() != StatusCode::UNAUTHORIZED {
        return Ok(initial);
    }

    let challenge = initial
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let Some(challenge) = challenge else {
        return Ok(initial);
    };

    let Some(challenge) = DigestChallenge::parse(&challenge) else {
        return Ok(initial);
    };

    let authorization = build_digest_authorization("GET", url, username, password, &challenge);
    client
        .get(url)
        .timeout(timeout)
        .header(AUTHORIZATION, authorization)
        .send()
        .await
}

pub async fn post_xml_with_auth(
    client: &reqwest::Client,
    url: &str,
    xml_body: &str,
    username: Option<&str>,
    password: Option<&str>,
    timeout_ms: u64,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let timeout = Duration::from_millis(timeout_ms);
    let (Some(username), Some(password)) = (username, password) else {
        return client
            .post(url)
            .timeout(timeout)
            .header(CONTENT_TYPE, "application/xml")
            .body(xml_body.to_string())
            .send()
            .await;
    };

    let initial = client
        .post(url)
        .timeout(timeout)
        .header(CONTENT_TYPE, "application/xml")
        .basic_auth(username, Some(password))
        .body(xml_body.to_string())
        .send()
        .await?;

    if initial.status() != StatusCode::UNAUTHORIZED {
        return Ok(initial);
    }

    let challenge = initial
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let Some(challenge) = challenge else {
        return Ok(initial);
    };

    let Some(challenge) = DigestChallenge::parse(&challenge) else {
        return Ok(initial);
    };

    let authorization = build_digest_authorization("POST", url, username, password, &challenge);
    client
        .post(url)
        .timeout(timeout)
        .header(CONTENT_TYPE, "application/xml")
        .header(AUTHORIZATION, authorization)
        .body(xml_body.to_string())
        .send()
        .await
}

#[derive(Debug, Clone)]
struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    qop: Option<String>,
    algorithm: Option<String>,
}

impl DigestChallenge {
    fn parse(header: &str) -> Option<Self> {
        let trimmed = header.trim_start();
        if !trimmed.to_ascii_lowercase().starts_with("digest ") {
            return None;
        }

        let params = trimmed
            .split_once(' ')
            .map(|(_, tail)| tail)
            .unwrap_or_default();
        let kv = parse_auth_kv(params);

        let realm = kv.get("realm")?.to_string();
        let nonce = kv.get("nonce")?.to_string();
        let opaque = kv.get("opaque").cloned();
        let qop = kv.get("qop").cloned();
        let algorithm = kv.get("algorithm").cloned();

        Some(Self {
            realm,
            nonce,
            opaque,
            qop,
            algorithm,
        })
    }
}

fn parse_auth_kv(input: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for token in split_tokens(input) {
        let Some((k, v)) = token.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let value = strip_quotes(v.trim());
        out.insert(key, value);
    }
    out
}

fn split_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                escaped = true;
                current.push(ch);
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    tokens.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        tokens.push(trimmed.to_string());
    }
    tokens
}

fn strip_quotes(input: &str) -> String {
    let mut s = input.trim().to_string();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_string();
    }
    s
}

fn build_digest_authorization(
    method: &str,
    url: &str,
    username: &str,
    password: &str,
    challenge: &DigestChallenge,
) -> String {
    let uri = request_uri(url);
    let nc = "00000001";
    let cnonce = generate_cnonce();
    let qop = select_qop(challenge.qop.as_deref());

    let mut ha1 = md5_hex(&format!("{username}:{}:{password}", challenge.realm));
    if let Some(algorithm) = &challenge.algorithm {
        if algorithm.eq_ignore_ascii_case("MD5-sess") {
            ha1 = md5_hex(&format!("{ha1}:{}:{cnonce}", challenge.nonce));
        }
    }
    let ha2 = md5_hex(&format!("{method}:{uri}"));

    let response = if let Some(qop_token) = &qop {
        md5_hex(&format!(
            "{ha1}:{}:{nc}:{cnonce}:{qop_token}:{ha2}",
            challenge.nonce
        ))
    } else {
        md5_hex(&format!("{ha1}:{}:{ha2}", challenge.nonce))
    };

    let mut params = vec![
        format!("username=\"{}\"", escape_quoted(username)),
        format!("realm=\"{}\"", escape_quoted(&challenge.realm)),
        format!("nonce=\"{}\"", escape_quoted(&challenge.nonce)),
        format!("uri=\"{}\"", escape_quoted(&uri)),
        format!("response=\"{response}\""),
    ];

    if let Some(algorithm) = &challenge.algorithm {
        params.push(format!("algorithm={algorithm}"));
    }
    if let Some(opaque) = &challenge.opaque {
        params.push(format!("opaque=\"{}\"", escape_quoted(opaque)));
    }
    if let Some(qop_token) = qop {
        params.push(format!("qop={qop_token}"));
        params.push(format!("nc={nc}"));
        params.push(format!("cnonce=\"{cnonce}\""));
    }

    format!("Digest {}", params.join(", "))
}

fn select_qop(qop_header: Option<&str>) -> Option<String> {
    let raw = qop_header?.to_ascii_lowercase();
    let tokens: Vec<_> = raw
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    if tokens.is_empty() {
        None
    } else if tokens.iter().any(|v| *v == "auth") {
        Some("auth".to_string())
    } else {
        Some(tokens[0].to_string())
    }
}

fn request_uri(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return "/".to_string();
    };
    match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    }
}

fn generate_cnonce() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = u128::from(std::process::id());
    format!("{:016x}", now ^ (pid << 32))
}

fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input.as_bytes()))
}

fn escape_quoted(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
