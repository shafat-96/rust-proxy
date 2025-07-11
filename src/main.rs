use actix_cors::Cors;
use actix_web::{
    get, middleware::Compress, web::Bytes, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use futures_util::stream::StreamExt;
use once_cell::sync::Lazy;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client,
};
use std::{collections::HashMap, str::FromStr};
use url::Url;

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("Failed to build reqwest client")
});

fn get_url(line: &str, base: &Url) -> Url {
    if let Ok(absolute) = Url::parse(line) {
        return absolute;
    }
    base.join(line).unwrap_or_else(|_| base.clone())
}

#[get("/")]
async fn m3u8_proxy(req: HttpRequest) -> impl Responder {
    let query: HashMap<String, String> = req
        .query_string()
        .split('&')
        .filter_map(|s| {
            let mut split = s.splitn(2, '=');
            Some((
                split.next()?.to_string(),
                urlencoding::decode(split.next().unwrap_or("")).ok()?.to_string(),
            ))
        })
        .collect();

    let target_url = match query.get("url") {
        Some(u) => u.clone(),
        None => return HttpResponse::BadRequest().body("Missing URL"),
    };

    let mut headers = HeaderMap::new();
    if let Some(header_json) = query.get("headers") {
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, String>>(header_json) {
            for (k, v) in parsed.into_iter() {
                if let Ok(name) = HeaderName::from_str(&k) {
                    if let Ok(value) = HeaderValue::from_str(&v) {
                        headers.insert(name, value);
                    }
                }
            }
        }
    }

    // Optional Range support
    if let Some(range) = req.headers().get("Range") {
        headers.insert("Range", range.clone());
    }

    let resp = match CLIENT.get(&target_url).headers(headers.clone()).send().await {
        Ok(r) => r,
        Err(_) => return HttpResponse::InternalServerError().body("Failed to fetch target URL"),
    };

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let is_m3u8 = target_url.ends_with(".m3u8")
        || content_type.contains("mpegurl")
        || content_type.contains("application/vnd.apple.mpegurl")
        || content_type.contains("application/x-mpegurl");

    if is_m3u8 {
        let m3u8_text = match resp.text().await {
            Ok(t) => t,
            Err(_) => return HttpResponse::InternalServerError().body("Failed to read m3u8"),
        };

        let scrape_url = Url::parse(&target_url).unwrap();
        let headers_param = query.get("headers").cloned();

        let lines: Vec<String> = m3u8_text
            .lines()
            .map(|line| {
                if line.starts_with('#') || line.trim().is_empty() {
                    if line.starts_with("#EXT-X-MAP:URI=\"") {
                        let inner_url = line
                            .trim_start_matches("#EXT-X-MAP:URI=\"")
                            .trim_end_matches('"');
                        let resolved = get_url(inner_url, &scrape_url);
                        let mut new_q = format!("url={}", resolved);
                        if let Some(ref h) = headers_param {
                            new_q.push_str(&format!("&headers={}", h));
                        }
                        return format!("#EXT-X-MAP:URI=\"/?{}\"", new_q);
                    }

                    if line.to_lowercase().contains("uri=") || line.to_lowercase().contains("url=")
                    {
                        let mut obj = HashMap::new();
                        let split: Vec<_> = line.splitn(2, ':').collect();
                        if split.len() != 2 {
                            return line.to_string();
                        }
                        let top = split[0];
                        for part in split[1].split(',') {
                            let kv: Vec<_> = part.splitn(2, '=').collect();
                            if kv.len() == 2 {
                                obj.insert(kv[0].trim(), kv[1].trim().trim_matches('"'));
                            }
                        }

                        for k in ["URI", "URL"] {
                            if let Some(url) = obj.get(k) {
                                let resolved = get_url(url, &scrape_url);
                                let mut new_q = format!("url={}", resolved);
                                if let Some(ref h) = headers_param {
                                    new_q.push_str(&format!("&headers={}", h));
                                }
                                let new_val = format!("/?{}", new_q);
                                obj.insert(k, Box::leak(new_val.into_boxed_str()));
                            }
                        }

                        let new_line = format!(
                            "{}:{}",
                            top,
                            obj.iter()
                                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                                .collect::<Vec<_>>()
                                .join(",")
                        );
                        return new_line;
                    }

                    return line.to_string();
                }

                let resolved = get_url(line, &scrape_url);
                let mut new_q = format!("url={}", resolved);
                if let Some(ref h) = headers_param {
                    new_q.push_str(&format!("&headers={}", h));
                }

                format!("/?{}", new_q)
            })
            .collect();

        return HttpResponse::Ok()
            .insert_header(("Access-Control-Allow-Origin", "*"))
            .insert_header(("Content-Type", "application/vnd.apple.mpegurl"))
            .body(lines.join("\n"));
    }

    // Stream non-m3u8 resources
    let stream = resp.bytes_stream().map(|chunk| {
        chunk.map(Bytes::from)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });

    HttpResponse::build(status)
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .insert_header(("Content-Type", content_type))
        .body(actix_web::body::BodyStream::new(stream))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("We alive bois: http://127.0.0.1:8080");

    HttpServer::new(|| {
        App::new()
            .wrap(Cors::permissive())
            .wrap(Compress::default())
            .service(m3u8_proxy)
    })
    .workers(num_cpus::get())
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
