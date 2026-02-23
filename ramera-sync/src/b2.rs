use std::io::Write;
use std::process::{Command, Stdio};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::config::B2Config;
use crate::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct B2Client {
    http: reqwest::Client,
    cfg: B2Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResult {
    pub file_id: String,
    pub file_name: String,
    pub content_length: u64,
}

#[derive(Debug, Clone)]
pub struct B2File {
    pub file_id: String,
    pub file_name: String,
}

impl B2Client {
    pub fn new(cfg: B2Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            cfg,
        }
    }

    pub async fn upload_named_bytes(
        &self,
        file_name: &str,
        content_type: &str,
        data: &[u8],
    ) -> Result<UploadResult> {
        if self.cfg.key_id.is_empty()
            || self.cfg.application_key.is_empty()
            || self.cfg.bucket_id.is_empty()
        {
            return Err(AppError::B2(
                "Missing B2 credentials (key_id, application_key, or bucket_id)".to_string(),
            ));
        }

        let auth = self.authorize().await?;
        let upload = self
            .get_upload_url(&auth.api_url, &auth.authorization_token)
            .await?;

        let sha1_hex = sha1_hex(data)?;
        let encoded_name = urlencoding::encode(&file_name);
        let response = self
            .http
            .post(upload.upload_url)
            .header(AUTHORIZATION, upload.authorization_token)
            .header("X-Bz-File-Name", encoded_name.as_ref())
            .header(CONTENT_TYPE, content_type)
            .header("X-Bz-Content-Sha1", &sha1_hex)
            .body(data.to_vec())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::B2(format!(
                "Upload failed with {status}: {}",
                compact(&body)
            )));
        }

        let body: UploadFileResponse = response.json().await?;
        if body.content_length != data.len() as u64 {
            let _ = self
                .delete_file_version(&body.file_id, &body.file_name)
                .await;
            return Err(AppError::B2(format!(
                "Upload verification failed for {}: content-length mismatch local={} remote={}",
                file_name,
                data.len(),
                body.content_length
            )));
        }
        if body.file_name != file_name {
            let _ = self
                .delete_file_version(&body.file_id, &body.file_name)
                .await;
            return Err(AppError::B2(format!(
                "Upload verification failed: remote file name mismatch local={} remote={}",
                file_name, body.file_name
            )));
        }
        if let Some(remote_sha1) = &body.content_sha1 {
            if remote_sha1 != "none" && !remote_sha1.eq_ignore_ascii_case(&sha1_hex) {
                let _ = self
                    .delete_file_version(&body.file_id, &body.file_name)
                    .await;
                return Err(AppError::B2(format!(
                    "Upload verification failed for {}: sha1 mismatch local={} remote={}",
                    file_name, sha1_hex, remote_sha1
                )));
            }
        }

        Ok(UploadResult {
            file_id: body.file_id,
            file_name: body.file_name,
            content_length: body.content_length,
        })
    }

    pub async fn file_exists(&self, file_name: &str) -> Result<bool> {
        let files = self.list_files(file_name).await?;
        Ok(files.iter().any(|f| f.file_name == file_name))
    }

    pub async fn download_named_bytes(&self, file_name: &str) -> Result<Vec<u8>> {
        if self.cfg.key_id.is_empty()
            || self.cfg.application_key.is_empty()
            || self.cfg.bucket_id.is_empty()
        {
            return Err(AppError::B2(
                "Missing B2 credentials (key_id, application_key, or bucket_id)".to_string(),
            ));
        }

        let files = self.list_files(file_name).await?;
        let Some(file) = files.into_iter().find(|f| f.file_name == file_name) else {
            return Err(AppError::B2(format!("B2 file not found: {file_name}")));
        };

        let auth = self.authorize().await?;
        let file_id = urlencoding::encode(&file.file_id);
        let response = self
            .http
            .get(format!(
                "{}/b2api/v2/b2_download_file_by_id?fileId={}",
                auth.download_url, file_id
            ))
            .header(AUTHORIZATION, auth.authorization_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::B2(format!(
                "Download failed with {status}: {}",
                compact(&body)
            )));
        }

        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }

    pub async fn list_files(&self, prefix: &str) -> Result<Vec<B2File>> {
        if self.cfg.key_id.is_empty()
            || self.cfg.application_key.is_empty()
            || self.cfg.bucket_id.is_empty()
        {
            return Err(AppError::B2(
                "Missing B2 credentials (key_id, application_key, or bucket_id)".to_string(),
            ));
        }

        let auth = self.authorize().await?;
        let mut out = Vec::new();
        let mut start_file_name: Option<String> = None;

        loop {
            let mut body = serde_json::json!({
                "bucketId": self.cfg.bucket_id,
                "prefix": prefix,
                "maxFileCount": 1000
            });
            if let Some(start) = &start_file_name {
                body["startFileName"] = serde_json::Value::String(start.clone());
            }

            let response = self
                .http
                .post(format!("{}/b2api/v2/b2_list_file_names", auth.api_url))
                .header(AUTHORIZATION, &auth.authorization_token)
                .json(&body)
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AppError::B2(format!(
                    "List file names failed with {status}: {}",
                    compact(&body)
                )));
            }

            let body: ListFileNamesResponse = response.json().await?;
            for file in body.files {
                out.push(B2File {
                    file_id: file.file_id,
                    file_name: file.file_name,
                });
            }

            if let Some(next) = body.next_file_name {
                start_file_name = Some(next);
            } else {
                break;
            }
        }

        Ok(out)
    }

    pub async fn delete_file_version(&self, file_id: &str, file_name: &str) -> Result<()> {
        if self.cfg.key_id.is_empty()
            || self.cfg.application_key.is_empty()
            || self.cfg.bucket_id.is_empty()
        {
            return Err(AppError::B2(
                "Missing B2 credentials (key_id, application_key, or bucket_id)".to_string(),
            ));
        }

        let auth = self.authorize().await?;
        let response = self
            .http
            .post(format!("{}/b2api/v2/b2_delete_file_version", auth.api_url))
            .header(AUTHORIZATION, auth.authorization_token)
            .json(&serde_json::json!({
                "fileId": file_id,
                "fileName": file_name
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::B2(format!(
                "Delete file version failed with {status}: {}",
                compact(&body)
            )));
        }

        Ok(())
    }

    async fn authorize(&self) -> Result<AuthorizeResponse> {
        let api_base = self
            .cfg
            .api_base
            .clone()
            .unwrap_or_else(|| "https://api.backblazeb2.com".to_string());

        let response = self
            .http
            .get(format!("{api_base}/b2api/v2/b2_authorize_account"))
            .basic_auth(&self.cfg.key_id, Some(&self.cfg.application_key))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::B2(format!(
                "Authorization failed with {status}: {}",
                compact(&body)
            )));
        }

        let body: AuthorizeResponse = response.json().await?;
        Ok(body)
    }

    async fn get_upload_url(
        &self,
        api_url: &str,
        auth_token: &str,
    ) -> Result<GetUploadUrlResponse> {
        let response = self
            .http
            .post(format!("{api_url}/b2api/v2/b2_get_upload_url"))
            .header(AUTHORIZATION, auth_token)
            .json(&serde_json::json!({
                "bucketId": self.cfg.bucket_id
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::B2(format!(
                "Get upload URL failed with {status}: {}",
                compact(&body)
            )));
        }

        let body: GetUploadUrlResponse = response.json().await?;
        Ok(body)
    }
}

fn compact(input: &str) -> String {
    let mut s = input.replace('\n', " ").replace('\r', " ");
    if s.len() > 220 {
        s.truncate(220);
    }
    s
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeResponse {
    authorization_token: String,
    api_url: String,
    download_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetUploadUrlResponse {
    upload_url: String,
    authorization_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadFileResponse {
    file_id: String,
    file_name: String,
    content_length: u64,
    content_sha1: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFileNamesResponse {
    files: Vec<ListFileItem>,
    next_file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFileItem {
    file_id: String,
    file_name: String,
}

fn sha1_hex(data: &[u8]) -> Result<String> {
    if let Ok(hash) = sha1_hex_with("sha1sum", &[], data) {
        return Ok(hash);
    }
    if let Ok(hash) = sha1_hex_with("shasum", &["-a", "1"], data) {
        return Ok(hash);
    }
    if let Ok(hash) = sha1_hex_with("openssl", &["sha1"], data) {
        return Ok(hash);
    }

    Err(AppError::Command(
        "no supported SHA1 tool found (tried: sha1sum, shasum, openssl)".to_string(),
    ))
}

fn sha1_hex_with(bin: &str, args: &[&str], data: &[u8]) -> Result<String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Command(format!("failed to start {bin}: {e}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(data).map_err(|e| {
            AppError::Command(format!("failed writing input for {bin} hash command: {e}"))
        })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| AppError::Command(format!("{bin} hash command failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Command(format!(
            "{bin} returned non-zero status: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(hash) = stdout
        .split(|c: char| c.is_whitespace() || c == '=' || c == ':' || c == ',')
        .find(|token| token.len() == 40 && token.chars().all(|c| c.is_ascii_hexdigit()))
    else {
        return Err(AppError::Command(format!(
            "{bin} produced invalid SHA1 output: {}",
            compact(&stdout)
        )));
    };
    if hash.len() != 40 {
        return Err(AppError::Command(format!(
            "{bin} produced invalid hash: {hash}"
        )));
    }
    Ok(hash.to_ascii_lowercase())
}
