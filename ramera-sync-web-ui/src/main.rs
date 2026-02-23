use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

static OP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct AppState {
    b2: B2Config,
}

#[derive(Clone, Debug)]
struct B2Config {
    key_id: String,
    application_key: String,
    bucket_id: String,
    file_prefix: String,
    api_base: String,
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
struct ListFileNamesResponse {
    files: Vec<ListFileItem>,
    next_file_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ListFileItem {
    file_id: String,
    file_name: String,
    #[serde(default)]
    content_length: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DownloadQuery {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DayQuery {
    camera: Option<String>,
}

#[derive(Default, Clone)]
struct DayStats {
    total_objects: usize,
    raw_count: usize,
    clip_count: usize,
    has_snapshot: bool,
    total_bytes: u64,
}

#[tokio::main]
async fn main() {
    let op = next_op_id();
    let cfg = match load_b2_config(op) {
        Ok(v) => v,
        Err(err) => {
            log_step(op, "startup.error", &err);
            std::process::exit(2);
        }
    };

    let bind = std::env::var("UI_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let addr: SocketAddr = match bind.parse() {
        Ok(v) => v,
        Err(err) => {
            log_step(
                op,
                "startup.error",
                &format!("invalid UI_BIND `{bind}`: {err}"),
            );
            std::process::exit(2);
        }
    };

    log_step(
        op,
        "startup.config",
        &format!(
            "bucket={} prefix={} bind={addr}",
            cfg.bucket_id,
            cfg.file_prefix.trim_end_matches('/')
        ),
    );

    let app = Router::new()
        .route("/", get(index_page))
        .route("/day/{day}", get(day_page))
        .route("/watch/day/{day}", get(watch_day_page))
        .route("/merged/day/{day}", get(merged_day_video))
        .route("/download/{file_id}", get(download_file))
        .with_state(AppState { b2: cfg });

    println!("ramera-sync-web-ui listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind failed");
    axum::serve(listener, app).await.expect("server failed");
}

async fn index_page(State(state): State<AppState>) -> Result<Html<String>, (StatusCode, String)> {
    let op = next_op_id();
    let started = Instant::now();
    log_step(op, "index.start", "loading day list from B2");

    let prefix_root = state.b2.file_prefix.trim_end_matches('/');
    let prefix = format!("{prefix_root}/records/");
    let files = list_files(op, &state.b2, &prefix).await?;

    let mut by_day: BTreeMap<String, DayStats> = BTreeMap::new();
    for file in &files {
        let Some(day) = extract_day_from_name(prefix_root, &file.file_name) else {
            continue;
        };
        let stats = by_day.entry(day).or_default();
        stats.total_objects += 1;
        stats.total_bytes = stats
            .total_bytes
            .saturating_add(file.content_length.unwrap_or(0));

        if file.file_name.contains("/raw/") {
            stats.raw_count += 1;
        } else if file.file_name.contains("/clips/") {
            stats.clip_count += 1;
        }

        if file.file_name.contains("/snapshot-") && file.file_name.ends_with(".json") {
            stats.has_snapshot = true;
        }
    }

    let total_days = by_day.len();
    let total_objects = by_day.values().map(|s| s.total_objects).sum::<usize>();
    let total_clips = by_day.values().map(|s| s.clip_count).sum::<usize>();
    let total_bytes = by_day.values().map(|s| s.total_bytes).sum::<u64>();

    let mut body = String::new();
    body.push_str("<div class=\"hero\"><h1>ramera-sync Web UI</h1><p>B2-backed browser for day/camera/chunk records.</p></div>");
    body.push_str("<div class=\"meta\">");
    body.push_str(&format!(
        "<span><strong>Bucket:</strong> <code>{}</code></span>",
        esc(&state.b2.bucket_id)
    ));
    body.push_str(&format!(
        "<span><strong>Prefix:</strong> <code>{}</code></span>",
        esc(prefix_root)
    ));
    body.push_str("</div>");

    body.push_str("<div class=\"cards\">");
    body.push_str(&metric_card("Days", &total_days.to_string()));
    body.push_str(&metric_card("Objects", &total_objects.to_string()));
    body.push_str(&metric_card("Clips", &total_clips.to_string()));
    body.push_str(&metric_card("Stored", &format_size(total_bytes)));
    body.push_str("</div>");

    body.push_str("<div class=\"toolbar\"><label for=\"dayFilter\">Filter days</label><input id=\"dayFilter\" type=\"search\" placeholder=\"YYYYMMDD\" oninput=\"filterDays()\"></div>");

    if by_day.is_empty() {
        body.push_str("<p class=\"note\">No records found under configured prefix.</p>");
    } else {
        body.push_str("<table id=\"daysTable\"><thead><tr><th>Day</th><th>Objects</th><th>Clips</th><th>Raw</th><th>Snapshot</th><th>Size</th></tr></thead><tbody>");
        for (day, stats) in by_day.iter().rev() {
            body.push_str(&format!(
                "<tr><td><a href=\"/day/{0}\">{0}</a></td><td>{1}</td><td>{2}</td><td>{3}</td><td>{4}</td><td>{5}</td></tr>",
                esc(day),
                stats.total_objects,
                stats.clip_count,
                stats.raw_count,
                if stats.has_snapshot { "yes" } else { "no" },
                format_size(stats.total_bytes)
            ));
        }
        body.push_str("</tbody></table>");
    }

    body.push_str(
        "<script>
function filterDays(){
  const q=document.getElementById('dayFilter').value.toLowerCase();
  const rows=document.querySelectorAll('#daysTable tbody tr');
  for(const r of rows){
    const day=r.children[0].innerText.toLowerCase();
    r.style.display=day.includes(q)?'':'none';
  }
}
</script>",
    );

    log_step(
        op,
        "index.done",
        &format!(
            "days={} objects={} clips={} elapsed={}ms",
            total_days,
            total_objects,
            total_clips,
            started.elapsed().as_millis()
        ),
    );

    Ok(Html(html_page("Days", &body)))
}

async fn day_page(
    State(state): State<AppState>,
    AxumPath(day): AxumPath<String>,
    Query(query): Query<DayQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    let op = next_op_id();
    let started = Instant::now();
    log_step(
        op,
        "day.start",
        &format!(
            "day={} camera_filter={}",
            day,
            query.camera.as_deref().unwrap_or("*")
        ),
    );

    if !is_day(&day) {
        return Err((StatusCode::BAD_REQUEST, "invalid day".to_string()));
    }

    let prefix_root = state.b2.file_prefix.trim_end_matches('/');
    let day_prefix = format!("{prefix_root}/records/{day}/");
    let mut files = list_files(op, &state.b2, &day_prefix).await?;
    files.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    let mut snapshot = Vec::new();
    let mut raw = Vec::new();
    let mut clips = Vec::new();
    let mut other = Vec::new();

    for f in files {
        if f.file_name.ends_with(&format!("snapshot-{day}.json")) {
            snapshot.push(f);
        } else if f.file_name.contains("/raw/") {
            raw.push(f);
        } else if f.file_name.contains("/clips/") {
            clips.push(f);
        } else {
            other.push(f);
        }
    }

    let mut by_camera: HashMap<String, Vec<ListFileItem>> = HashMap::new();
    for file in clips {
        let name = file_name_only(&file.file_name);
        let cam = camera_key_from_clip_name(name);
        by_camera.entry(cam).or_default().push(file);
    }

    let mut cameras: Vec<_> = by_camera.keys().cloned().collect();
    cameras.sort();

    let mut body = String::new();
    body.push_str(&format!("<div class=\"hero\"><h1>Day {}</h1><p>Browse snapshot, raw payloads, and clip chunks.</p></div>", esc(&day)));
    body.push_str("<div class=\"toolbar\">");
    body.push_str("<a class=\"btn\" href=\"/\">Back</a>");
    body.push_str(&format!(
        "<a class=\"btn\" href=\"/watch/day/{}\">Watch merged day</a>",
        esc(&day)
    ));
    body.push_str("<form method=\"get\" class=\"inline\"><label for=\"camera\">Camera</label><select id=\"camera\" name=\"camera\"><option value=\"\">All</option>");
    for cam in &cameras {
        let selected = query.camera.as_deref() == Some(cam.as_str());
        body.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            esc(cam),
            if selected { " selected" } else { "" },
            esc(cam)
        ));
    }
    body.push_str("</select><button type=\"submit\">Apply</button></form>");
    body.push_str("</div>");

    let total_clip_count = by_camera.values().map(|v| v.len()).sum::<usize>();
    let total_clip_bytes = by_camera
        .values()
        .flat_map(|v| v.iter())
        .map(|f| f.content_length.unwrap_or(0))
        .sum::<u64>();

    body.push_str("<div class=\"cards\">");
    body.push_str(&metric_card("Snapshot", &snapshot.len().to_string()));
    body.push_str(&metric_card("Raw", &raw.len().to_string()));
    body.push_str(&metric_card("Clip Chunks", &total_clip_count.to_string()));
    body.push_str(&metric_card("Clip Size", &format_size(total_clip_bytes)));
    body.push_str("</div>");

    body.push_str("<h2>Snapshot</h2>");
    body.push_str(&render_file_table(&snapshot));

    body.push_str("<h2>Raw</h2>");
    body.push_str(&render_file_table(&raw));

    body.push_str("<h2>Clips by Camera</h2>");
    if cameras.is_empty() {
        body.push_str("<p class=\"note\">No clips for this day.</p>");
    } else {
        for cam in cameras {
            if let Some(filter) = query.camera.as_deref() {
                if cam != filter {
                    continue;
                }
            }
            let list = by_camera.get(&cam).cloned().unwrap_or_default();
            let cam_bytes = list
                .iter()
                .map(|f| f.content_length.unwrap_or(0))
                .sum::<u64>();
            let cam_q = urlencoding::encode(&cam);
            body.push_str(&format!(
                "<h3>{}</h3><p class=\"note\">{} clip(s), {} <a class=\"btn btn-small\" href=\"/watch/day/{}?camera={}\">Watch merged</a></p>",
                esc(&cam),
                list.len(),
                format_size(cam_bytes),
                esc(&day),
                cam_q
            ));
            body.push_str(&render_file_table(&list));
        }
    }

    body.push_str("<h2>Other</h2>");
    body.push_str(&render_file_table(&other));

    log_step(
        op,
        "day.done",
        &format!(
            "day={} snapshot={} raw={} cameras={} elapsed={}ms",
            day,
            snapshot.len(),
            raw.len(),
            by_camera.len(),
            started.elapsed().as_millis()
        ),
    );

    Ok(Html(html_page(&format!("Day {day}"), &body)))
}

async fn watch_day_page(
    AxumPath(day): AxumPath<String>,
    Query(query): Query<DayQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    if !is_day(&day) {
        return Err((StatusCode::BAD_REQUEST, "invalid day".to_string()));
    }

    let src = if let Some(cam) = query.camera.as_deref().filter(|v| !v.trim().is_empty()) {
        format!("/merged/day/{}?camera={}", day, urlencoding::encode(cam))
    } else {
        format!("/merged/day/{day}")
    };

    let mut body = String::new();
    body.push_str(&format!(
        "<div class=\"hero\"><h1>Watch Day {}</h1><p>Merged playback for this day{}</p></div>",
        esc(&day),
        query
            .camera
            .as_deref()
            .map(|c| format!(" (camera: {})", esc(c)))
            .unwrap_or_default()
    ));
    body.push_str("<div class=\"toolbar\">");
    body.push_str(&format!(
        "<a class=\"btn\" href=\"/day/{}\">Back to day</a>",
        esc(&day)
    ));
    body.push_str(&format!(
        "<a class=\"btn\" href=\"{}\">Open stream directly</a>",
        esc(&src)
    ));
    body.push_str("</div>");
    body.push_str(&format!(
        "<video controls autoplay preload=\"metadata\" style=\"width:100%;max-height:78vh;background:#000;border-radius:12px\" src=\"{}\"></video>",
        esc(&src)
    ));
    body.push_str("<p class=\"note\">If playback does not start immediately, wait for merge generation to finish and reload.</p>");

    Ok(Html(html_page(&format!("Watch {day}"), &body)))
}

async fn merged_day_video(
    State(state): State<AppState>,
    AxumPath(day): AxumPath<String>,
    Query(query): Query<DayQuery>,
) -> Result<Response, (StatusCode, String)> {
    let op = next_op_id();
    let started = Instant::now();
    log_step(
        op,
        "merge.start",
        &format!(
            "day={} camera={}",
            day,
            query.camera.as_deref().unwrap_or("*")
        ),
    );

    if !is_day(&day) {
        return Err((StatusCode::BAD_REQUEST, "invalid day".to_string()));
    }

    let camera = query
        .camera
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let (merged_path, merged_name) =
        prepare_merged_day_file(op, &state.b2, &day, camera.as_deref()).await?;
    let bytes = std::fs::read(&merged_path).map_err(|e| {
        log_step(op, "merge.read_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read merged file {}: {e}", merged_path.display()),
        )
    })?;

    log_step(
        op,
        "merge.done",
        &format!(
            "file={} size={} elapsed={}ms",
            merged_name,
            format_size(bytes.len() as u64),
            started.elapsed().as_millis()
        ),
    );

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{}\"", merged_name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }

    Ok((headers, bytes).into_response())
}

async fn download_file(
    State(state): State<AppState>,
    AxumPath(file_id): AxumPath<String>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, (StatusCode, String)> {
    let op = next_op_id();
    let started = Instant::now();
    let name = query.name.as_deref().map_or("file.bin", |v| v).to_string();
    log_step(
        op,
        "download.start",
        &format!("file_id={} name={}", file_id, name),
    );

    let auth = authorize(op, &state.b2).await?;
    let bytes = download_file_by_id(op, &auth, &file_id).await?;
    log_step(
        op,
        "download.done",
        &format!(
            "name={} size={} elapsed={}ms",
            name,
            format_size(bytes.len() as u64),
            started.elapsed().as_millis()
        ),
    );

    let mut headers = HeaderMap::new();
    let content_type = detect_content_type(&name);
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{}\"", name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }

    Ok((headers, bytes).into_response())
}

async fn authorize(op: u64, cfg: &B2Config) -> Result<AuthorizeResponse, (StatusCode, String)> {
    let url = format!("{}/b2api/v2/b2_authorize_account", cfg.api_base);
    log_step(op, "b2.authorize.request", &url);

    let resp = reqwest::Client::new()
        .get(url)
        .basic_auth(&cfg.key_id, Some(&cfg.application_key))
        .send()
        .await
        .map_err(|e| map_http_err(op, "b2.authorize.http_error", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        log_step(
            op,
            "b2.authorize.fail",
            &format!("status={} body={}", status, compact(&body)),
        );
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("b2 authorize failed with {status}: {}", compact(&body)),
        ));
    }

    let body = resp
        .json::<AuthorizeResponse>()
        .await
        .map_err(|e| map_http_err(op, "b2.authorize.decode_error", e))?;
    log_step(op, "b2.authorize.ok", "authorized");
    Ok(body)
}

async fn list_files(
    op: u64,
    cfg: &B2Config,
    prefix: &str,
) -> Result<Vec<ListFileItem>, (StatusCode, String)> {
    log_step(op, "b2.list.start", &format!("prefix={}", prefix));
    let auth = authorize(op, cfg).await?;

    let mut out = Vec::new();
    let mut start_file_name: Option<String> = None;
    let mut pages = 0usize;

    loop {
        let mut body = serde_json::json!({
            "bucketId": cfg.bucket_id,
            "prefix": prefix,
            "maxFileCount": 1000
        });
        if let Some(start) = &start_file_name {
            body["startFileName"] = serde_json::Value::String(start.clone());
        }

        let resp = reqwest::Client::new()
            .post(format!("{}/b2api/v2/b2_list_file_names", auth.api_url))
            .header(header::AUTHORIZATION, &auth.authorization_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| map_http_err(op, "b2.list.http_error", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            log_step(
                op,
                "b2.list.fail",
                &format!("status={} body={}", status, compact(&body)),
            );
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("b2 list failed with {status}: {}", compact(&body)),
            ));
        }

        let page = resp
            .json::<ListFileNamesResponse>()
            .await
            .map_err(|e| map_http_err(op, "b2.list.decode_error", e))?;
        pages += 1;
        log_step(
            op,
            "b2.list.page",
            &format!("page={} files={}", pages, page.files.len()),
        );

        out.extend(page.files);

        if let Some(next) = page.next_file_name {
            start_file_name = Some(next);
        } else {
            break;
        }
    }

    log_step(
        op,
        "b2.list.done",
        &format!("files={} pages={}", out.len(), pages),
    );
    Ok(out)
}

fn render_file_table(files: &[ListFileItem]) -> String {
    if files.is_empty() {
        return "<p class=\"note\">None</p>".to_string();
    }

    let mut html = String::new();
    html.push_str(
        "<table><thead><tr><th>Name</th><th>Size</th><th>Action</th></tr></thead><tbody>",
    );
    for file in files {
        let name = file_name_only(&file.file_name);
        let file_id_enc = urlencoding::encode(&file.file_id);
        let name_enc = urlencoding::encode(name);
        let size = format_size(file.content_length.unwrap_or(0));
        html.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td><a class=\"btn btn-small\" href=\"/download/{}?name={}\">Open/Download</a></td></tr>",
            esc(name),
            size,
            file_id_enc,
            name_enc
        ));
    }
    html.push_str("</tbody></table>");
    html
}

fn file_name_only(full: &str) -> &str {
    full.rsplit('/').next().unwrap_or(full)
}

fn camera_key_from_clip_name(name: &str) -> String {
    name.split("_track").next().unwrap_or("unknown").to_string()
}

fn extract_day_from_name(prefix_root: &str, file_name: &str) -> Option<String> {
    let records_prefix = format!("{}/records/", prefix_root.trim_end_matches('/'));
    let tail = file_name.strip_prefix(&records_prefix)?;
    let day = tail.split('/').next()?;
    if is_day(day) {
        Some(day.to_string())
    } else {
        None
    }
}

fn is_day(day: &str) -> bool {
    day.len() == 8 && day.chars().all(|c| c.is_ascii_digit())
}

fn detect_content_type(name: &str) -> &'static str {
    if name.ends_with(".mkv") {
        "video/x-matroska"
    } else if name.ends_with(".mp4") {
        "video/mp4"
    } else if name.ends_with(".json") {
        "application/json"
    } else if name.ends_with(".xml") {
        "application/xml"
    } else if name.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

fn load_b2_config(op: u64) -> Result<B2Config, String> {
    let mut vals = HashMap::new();

    let config_path =
        std::env::var("RAMERA_SYNC_CONFIG").unwrap_or_else(|_| "settings.conf".to_string());
    log_step(op, "config.load", &format!("path={config_path}"));

    if Path::new(&config_path).exists() {
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("failed to read {config_path}: {e}"))?;
        for line in raw.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
                continue;
            }
            if let Some((k, v)) = t.split_once('=') {
                vals.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        log_step(op, "config.loaded", &format!("keys={}", vals.len()));
    } else {
        log_step(
            op,
            "config.missing",
            "settings.conf not found, env-only mode",
        );
    }

    let key_id = env_or_map("B2_KEY_ID", &vals, "b2.key_id")?;
    let application_key = env_or_map("B2_APPLICATION_KEY", &vals, "b2.application_key")?;
    let bucket_id = env_or_map("B2_BUCKET_ID", &vals, "b2.bucket_id")?;
    let file_prefix = std::env::var("B2_FILE_PREFIX")
        .ok()
        .or_else(|| vals.get("b2.file_prefix").cloned())
        .map(|v| resolve_env_token(&v))
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "ramera/nvr-snapshots".to_string());
    let api_base = std::env::var("B2_API_BASE")
        .ok()
        .or_else(|| vals.get("b2.api_base").cloned())
        .map(|v| resolve_env_token(&v))
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://api.backblazeb2.com".to_string());

    Ok(B2Config {
        key_id,
        application_key,
        bucket_id,
        file_prefix,
        api_base,
    })
}

fn env_or_map(
    env_key: &str,
    vals: &HashMap<String, String>,
    map_key: &str,
) -> Result<String, String> {
    if let Ok(v) = std::env::var(env_key) {
        if !v.trim().is_empty() {
            return Ok(v);
        }
    }
    if let Some(v) = vals.get(map_key) {
        let resolved = resolve_env_token(v);
        if !resolved.trim().is_empty() {
            return Ok(resolved);
        }
    }
    Err(format!("missing required config: {env_key} or {map_key}"))
}

fn resolve_env_token(value: &str) -> String {
    if value.starts_with("${") && value.ends_with('}') && value.len() > 3 {
        let key = &value[2..value.len() - 1];
        return std::env::var(key).unwrap_or_default();
    }
    value.to_string()
}

fn internal_http(err: reqwest::Error) -> (StatusCode, String) {
    (StatusCode::BAD_GATEWAY, format!("http error: {err}"))
}

fn map_http_err(op: u64, step: &str, err: reqwest::Error) -> (StatusCode, String) {
    log_step(op, step, &err.to_string());
    internal_http(err)
}

fn compact(input: &str) -> String {
    let mut s = input.replace('\n', " ").replace('\r', " ");
    if s.len() > 220 {
        s.truncate(220);
    }
    s
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;

    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn metric_card(label: &str, value: &str) -> String {
    format!(
        "<div class=\"card\"><div class=\"k\">{}</div><div class=\"v\">{}</div></div>",
        esc(label),
        esc(value)
    )
}

fn html_page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>
:root{{--bg:#f8f9f4;--ink:#15201a;--muted:#4f5f53;--panel:#ffffff;--accent:#1f6f4a;--line:#dbe4dd;--shadow:0 8px 24px rgba(8,20,14,.08)}}
*{{box-sizing:border-box}}
body{{margin:0;background:linear-gradient(180deg,#edf5ef 0,#f8f9f4 26%);color:var(--ink);font-family:'IBM Plex Sans','Segoe UI',sans-serif;line-height:1.45}}
main{{max-width:1080px;margin:24px auto;padding:0 14px 28px}}
.hero{{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:16px 18px;box-shadow:var(--shadow)}}
.hero h1{{margin:0 0 4px;font-size:1.45rem}}
.hero p{{margin:0;color:var(--muted)}}
.meta{{display:flex;flex-wrap:wrap;gap:10px;margin:12px 0 16px;color:var(--muted)}}
.meta code{{background:#f1f5f2;border:1px solid var(--line);padding:1px 6px;border-radius:6px}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;margin:12px 0 16px}}
.card{{background:var(--panel);border:1px solid var(--line);border-radius:12px;padding:10px 12px}}
.card .k{{font-size:.78rem;color:var(--muted);text-transform:uppercase;letter-spacing:.04em}}
.card .v{{font-size:1.18rem;font-weight:700}}
.toolbar{{display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin:12px 0}}
.inline{{display:flex;gap:8px;align-items:center;flex-wrap:wrap}}
input,select,button{{font:inherit;padding:7px 9px;border:1px solid #cfd9d2;border-radius:8px;background:#fff}}
button,.btn{{background:var(--accent);color:#fff;border:0;padding:8px 12px;border-radius:8px;text-decoration:none;display:inline-block;cursor:pointer}}
.btn-small{{padding:5px 8px;font-size:.92rem}}
a{{color:#0f5b3c}}
a:hover{{opacity:.9}}
h2{{margin:18px 0 8px}}
h3{{margin:14px 0 6px}}
table{{width:100%;border-collapse:separate;border-spacing:0;background:var(--panel);border:1px solid var(--line);border-radius:12px;overflow:hidden;box-shadow:var(--shadow)}}
th,td{{padding:9px 10px;border-bottom:1px solid #edf1ee;text-align:left;vertical-align:top}}
th{{background:#f1f5f2;color:#234235;font-weight:700}}
tr:last-child td{{border-bottom:none}}
code{{background:#f1f5f2;border:1px solid var(--line);padding:1px 5px;border-radius:6px;font-size:.92em}}
.note{{color:var(--muted)}}
@media (max-width:760px){{main{{padding:0 10px 18px}}th,td{{padding:8px}}.hero h1{{font-size:1.25rem}}}}
</style></head><body><main>{}</main></body></html>",
        esc(title), body
    )
}

fn next_op_id() -> u64 {
    OP_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn log_step(op: u64, step: &str, detail: &str) {
    let ts = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(v) => v.as_secs(),
        Err(_) => 0,
    };
    eprintln!("[ui][{}][op:{}] {} | {}", ts, op, step, detail);
}

async fn download_file_by_id(
    op: u64,
    auth: &AuthorizeResponse,
    file_id: &str,
) -> Result<Vec<u8>, (StatusCode, String)> {
    let encoded_id = urlencoding::encode(file_id);

    let mut bases = vec![
        auth.api_url.trim_end_matches('/').to_string(),
        auth.download_url.trim_end_matches('/').to_string(),
    ];
    if bases.get(0) == bases.get(1) {
        let _ = bases.pop();
    }

    let client = reqwest::Client::new();
    let mut last_error = String::new();

    for (idx, base) in bases.iter().enumerate() {
        let url = format!("{base}/b2api/v2/b2_download_file_by_id?fileId={encoded_id}");
        log_step(
            op,
            "download.request",
            &format!("attempt={} {}", idx + 1, url),
        );

        let resp = match client
            .get(&url)
            .header(header::AUTHORIZATION, &auth.authorization_token)
            .send()
            .await
        {
            Ok(v) => v,
            Err(err) => {
                last_error = err.to_string();
                log_step(
                    op,
                    "download.http_error",
                    &format!("attempt={} error={}", idx + 1, last_error),
                );
                continue;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            last_error = format!("status={status} body={}", compact(&body));
            log_step(
                op,
                "download.fail",
                &format!("attempt={} {}", idx + 1, last_error),
            );
            continue;
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| map_http_err(op, "download.read_error", e))?;
        return Ok(bytes.to_vec());
    }

    if last_error.is_empty() {
        last_error = "no download endpoint attempted".to_string();
    }

    Err((
        StatusCode::BAD_GATEWAY,
        format!("b2 download failed for file id {file_id}: {last_error}"),
    ))
}

async fn prepare_merged_day_file(
    op: u64,
    cfg: &B2Config,
    day: &str,
    camera: Option<&str>,
) -> Result<(PathBuf, String), (StatusCode, String)> {
    let prefix_root = cfg.file_prefix.trim_end_matches('/');
    let day_prefix = format!("{prefix_root}/records/{day}/");
    let mut files = list_files(op, cfg, &day_prefix).await?;
    files.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    let mut clips: Vec<ListFileItem> = files
        .into_iter()
        .filter(|f| f.file_name.contains("/clips/"))
        .collect();

    if let Some(cam) = camera {
        clips.retain(|f| camera_key_from_clip_name(file_name_only(&f.file_name)) == cam);
    }

    if clips.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            format!(
                "no clips found for day {}{}",
                day,
                camera
                    .map(|c| format!(" and camera {}", c))
                    .unwrap_or_default()
            ),
        ));
    }

    let camera_token = sanitize_filename_token(camera.unwrap_or("all"));
    let merged_name = format!("merged-{day}-{camera_token}.mp4");
    let cache_root =
        std::env::var("UI_CACHE_DIR").unwrap_or_else(|_| ".ramera-sync-web-ui-cache".to_string());
    let cache_dir = Path::new(&cache_root).join("merged").join(day);
    std::fs::create_dir_all(&cache_dir).map_err(|e| {
        log_step(op, "merge.cache_dir_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create cache dir {}: {e}", cache_dir.display()),
        )
    })?;

    let merged_path = cache_dir.join(&merged_name);
    let meta_path = cache_dir.join(format!("{merged_name}.meta"));
    let signature = build_merge_signature(&clips);

    if merged_path.exists() {
        if let Ok(existing_sig) = std::fs::read_to_string(&meta_path) {
            if existing_sig == signature {
                log_step(
                    op,
                    "merge.cache_hit",
                    &format!("path={} clips={}", merged_path.display(), clips.len()),
                );
                return Ok((merged_path, merged_name));
            }
        }
    }

    let work_dir = Path::new(&cache_root)
        .join("work")
        .join(format!("merge-{}-{}-{op}", day, camera_token));
    if work_dir.exists() {
        let _ = std::fs::remove_dir_all(&work_dir);
    }
    std::fs::create_dir_all(&work_dir).map_err(|e| {
        log_step(op, "merge.work_dir_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create work dir {}: {e}", work_dir.display()),
        )
    })?;

    let auth = authorize(op, cfg).await?;
    let mut concat = String::new();

    for (idx, file) in clips.iter().enumerate() {
        let part_path = work_dir.join(format!("{:05}.mkv", idx + 1));
        let bytes = download_file_by_id(op, &auth, &file.file_id).await?;
        std::fs::write(&part_path, bytes).map_err(|e| {
            log_step(op, "merge.write_part_error", &e.to_string());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to write clip part {}: {e}", part_path.display()),
            )
        })?;
        let abs = absolute_path(&part_path).map_err(|e| {
            log_step(op, "merge.path_error", &e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;
        concat.push_str(&format!("file '{}'\n", escape_concat_path(&abs)));
    }

    let list_path = work_dir.join("concat.list");
    std::fs::write(&list_path, concat).map_err(|e| {
        log_step(op, "merge.concat_list_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write concat list {}: {e}", list_path.display()),
        )
    })?;

    let ffmpeg_bin = resolve_ffmpeg_bin().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "ffmpeg not found (set FFMPEG_BIN or install ffmpeg)".to_string(),
        )
    })?;
    let out_tmp = work_dir.join("merged.tmp.mp4");
    let status = Command::new(&ffmpeg_bin)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&list_path)
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&out_tmp)
        .status()
        .map_err(|e| {
            log_step(op, "merge.ffmpeg_start_error", &e.to_string());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to start ffmpeg: {e}"),
            )
        })?;

    if !status.success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("ffmpeg merge failed with status {status}"),
        ));
    }

    std::fs::rename(&out_tmp, &merged_path).map_err(|e| {
        log_step(op, "merge.rename_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "failed to move merged output {} -> {}: {e}",
                out_tmp.display(),
                merged_path.display()
            ),
        )
    })?;
    std::fs::write(&meta_path, signature).map_err(|e| {
        log_step(op, "merge.meta_error", &e.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "failed to write merge metadata {}: {e}",
                meta_path.display()
            ),
        )
    })?;

    let _ = std::fs::remove_dir_all(&work_dir);
    Ok((merged_path, merged_name))
}

fn build_merge_signature(files: &[ListFileItem]) -> String {
    let mut out = format!("count={}\n", files.len());
    for file in files {
        out.push_str(&format!(
            "{}|{}|{}\n",
            file.file_id,
            file.file_name,
            file.content_length.unwrap_or(0)
        ));
    }
    out
}

fn resolve_ffmpeg_bin() -> Option<String> {
    if let Ok(path) = std::env::var("FFMPEG_BIN") {
        if !path.trim().is_empty() {
            return Some(path);
        }
    }

    let local = PathBuf::from("ffmpeg").join("ffmpeg");
    if local.exists() {
        return Some(local.display().to_string());
    }

    if has_binary("ffmpeg") {
        return Some("ffmpeg".to_string());
    }

    None
}

fn has_binary(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| format!("failed to read current dir: {e}"))
}

fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

fn sanitize_filename_token(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "all".to_string()
    } else {
        out
    }
}
