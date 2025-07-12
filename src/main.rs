use actix_cors::Cors;
use actix_web::{
    get, http::header, middleware::Compress, web::{Query}, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use actix_web::body::BodyStream;
use once_cell::sync::Lazy;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client,
};
use serde::Deserialize;
use std::{collections::{HashMap, HashSet}, time::Duration};
use url::Url;
use futures_util::TryStreamExt;
use regex::Regex;
use std::env;

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .pool_idle_timeout(Duration::from_secs(30))
        .http2_adaptive_window(true)
        .build()
        .unwrap_or_else(|e| {
            eprintln!("Failed to build reqwest client: {}", e);
            Client::new()
        })
});

static M3U8_MIME_TYPES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    HashSet::from([
        "mpegurl",
        "application/vnd.apple.mpegurl",
        "application/x-mpegurl",
    ])
});

static URI_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(URI|URL)="([^"]+)""#).unwrap()
});

// Static CORS enable flag
static ENABLE_CORS: Lazy<bool> = Lazy::new(|| {
    env::var("ENABLE_CORS")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false)
});

// Static allowed origins for CORS
static ALLOWED_ORIGINS: Lazy<[&str; 3]> = Lazy::new(|| [
    "http://localhost:5173",
    "http://localhost:3000",
    "http://aniwave.at",
]);

// Query parameters structure
#[derive(Deserialize)]
struct QueryParams {
    url: String,
    headers: Option<String>,
    origin: Option<String>,
}

// Resolve relative or absolute URLs
fn get_url(line: &str, base: &Url) -> Result<Url, url::ParseError> {
    if let Ok(absolute) = Url::parse(line) {
        Ok(absolute)
    } else {
        base.join(line)
    }
}

// Check if the request origin is allowed
fn is_allowed_origin(req: &HttpRequest) -> bool {
    if !*ENABLE_CORS {
        return true; // Allow all origins if CORS restrictions are disabled
    }

    // Check Origin header
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        if let Ok(origin_str) = origin.to_str() {
            return ALLOWED_ORIGINS.contains(&origin_str);
        }
        return false;
    }

    // Check Referer as fallback
    if let Some(referer) = req.headers().get(header::REFERER) {
        if let Ok(referer_str) = referer.to_str() {
            return ALLOWED_ORIGINS.iter().any(|allowed| referer_str.starts_with(allowed));
        }
    }

    false
}

#[get("/")]
async fn m3u8_proxy(req: HttpRequest) -> impl Responder {
    // Check origin before processing request
    if !is_allowed_origin(&req) {
        return HttpResponse::Forbidden()
            .insert_header((header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"))
            .body("Access denied: Origin not allowed");
    }

    // Get the actual origin from the request
    let origin = req.headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("*");

    // Parse query parameters
    let query = match Query::<QueryParams>::from_query(req.query_string()) {
        Ok(q) => q,
        Err(_) => return HttpResponse::BadRequest().body("Invalid query parameters"),
    };

    // Validate target URL
    let target_url = match Url::parse(&query.url) {
        Ok(url) => url,
        Err(_) => return HttpResponse::BadRequest().body("Invalid URL format"),
    };

    // Build headers
    let mut headers = HeaderMap::new();
    if let Some(header_json) = &query.headers {
        match serde_json::from_str::<HashMap<String, String>>(header_json) {
            Ok(parsed) => {
                if parsed.len() > 50 {
                    return HttpResponse::BadRequest().body("Too many headers");
                }
                for (k, v) in parsed.into_iter() {
                    match (HeaderName::try_from(k.as_str()), HeaderValue::from_str(&v)) {
                        (Ok(name), Ok(value)) => {
                            headers.insert(name, value);
                        }
                        _ => return HttpResponse::BadRequest().body("Invalid header format"),
                    }
                }
            }
            Err(_) => return HttpResponse::BadRequest().body("Invalid headers JSON"),
        }
    }

    // Add Origin header
    if let Some(origin) = &query.origin {
        match HeaderValue::from_str(origin) {
            Ok(origin_value) => headers.insert("Origin", origin_value),
            Err(_) => return HttpResponse::BadRequest().body("Invalid Origin header"),
        };
    }

    // Add Range header
    if let Some(range) = req.headers().get("Range") {
        headers.insert("Range", range.clone());
    }

    // Send request to target URL
    let resp = match CLIENT
        .get(target_url.as_str())
        .headers(headers)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return HttpResponse::InternalServerError().body(format!("Failed to fetch target URL: {}", e)),
    };

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Check if response is an m3u8 playlist
    let is_m3u8 = target_url.path().ends_with(".m3u8")
        || M3U8_MIME_TYPES.iter().any(|&mime| content_type.contains(mime));

    if is_m3u8 {
        // Read and validate m3u8 text
        let m3u8_text = match resp.text().await {
            Ok(t) => {
                if t.len() > 10_000_000 {
                    return HttpResponse::BadRequest().body("m3u8 file too large");
                }
                t
            }
            Err(_) => return HttpResponse::InternalServerError().body("Failed to read m3u8"),
        };

        // Process m3u8 lines
        let lines: Vec<String> = m3u8_text
            .lines()
            .take(200_000) // Increased line limit as per your update
            .map(|line| {
                if line.starts_with('#') || line.trim().is_empty() {
                    if line.starts_with("#EXT-X-MAP:URI=\"") {
                        let inner_url = line
                            .trim_start_matches("#EXT-X-MAP:URI=\"")
                            .trim_end_matches('"');
                        let resolved = match get_url(inner_url, &target_url) {
                            Ok(url) => url,
                            Err(_) => return line.to_string(),
                        };
                        let mut new_q = format!("url={}", urlencoding::encode(resolved.as_str()));
                        if let Some(h) = &query.headers {
                            new_q.push_str(&format!("&headers={}", h));
                        }
                        return format!("#EXT-X-MAP:URI=\"/?{}\"", new_q);
                    }

                    if URI_REGEX.is_match(line) {
                        let mut new_line = line.to_string();
                        if let Some(caps) = URI_REGEX.captures(line) {
                            let key = &caps[1];
                            let url = &caps[2];
                            let resolved = match get_url(url, &target_url) {
                                Ok(url) => url,
                                Err(_) => return line.to_string(),
                            };
                            let mut new_q = format!("url={}", urlencoding::encode(resolved.as_str()));
                            if let Some(h) = &query.headers {
                                new_q.push_str(&format!("&headers={}", h));
                            }
                            new_line = URI_REGEX
                                .replace(&new_line, format!(r#"{}="/?{}""#, key, new_q))
                                .to_string();
                        }
                        return new_line;
                    }

                    return line.to_string();
                }

                let resolved = match get_url(line, &target_url) {
                    Ok(url) => url,
                    Err(_) => return line.to_string(),
                };
                let mut new_q = format!("url={}", urlencoding::encode(resolved.as_str()));
                if let Some(h) = &query.headers {
                    new_q.push_str(&format!("&headers={}", h));
                }
                format!("/?{}", new_q)
            })
            .collect();

        return HttpResponse::Ok()
            .insert_header((header::ACCESS_CONTROL_ALLOW_ORIGIN, origin))
            .insert_header(("Content-Type", "application/vnd.apple.mpegurl"))
            .body(lines.join("\n"));
    }

    // Stream non-m3u8 resources
    let stream = resp
        .bytes_stream()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    HttpResponse::build(status)
        .insert_header((header::ACCESS_CONTROL_ALLOW_ORIGIN, origin))
        .insert_header(("Content-Type", content_type))
        .body(BodyStream::new(stream))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok(); 
    println!("Server running at: http://127.0.0.1:8080");
    println!("CORS enabled: {}, Allowed origins: {:?}", *ENABLE_CORS, *ALLOWED_ORIGINS);

    HttpServer::new(|| {
        let cors = if *ENABLE_CORS {
            Cors::default()
                .allowed_origin("http://localhost:5173")
                .allowed_origin("http://localhost:3000")
                .allowed_origin("http://aniwave.at")
                .allowed_methods(vec!["GET"])
                .allowed_headers(vec![header::AUTHORIZATION, header::ACCEPT, header::ORIGIN])
                .max_age(3600)
                .supports_credentials()
        } else {
            Cors::permissive()
        };

        App::new()
            .wrap(cors)
            .wrap(Compress::default())
            .service(m3u8_proxy)
    })
    .workers(num_cpus::get())
    .bind("0.0.0.0:8080")?
    .run()
    .await
}