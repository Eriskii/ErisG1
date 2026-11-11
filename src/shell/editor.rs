//! The layout editor: a page at http://127.0.0.1:4141 to arrange the tiles, with a picture of what the glasses would
//! show (the layout run on a simulated arm, `sim::render`), and a button that saves the layout and puts it on the
//! glasses. Only this computer can reach it: it listens on 127.0.0.1 and takes requests only for that host.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use super::layout::{Layout, Scene};
use super::{Command, Shared};

const PAGE: &str = include_str!("editor.html");
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 1 << 20;

pub(crate) async fn serve(
    listener: TcpListener,
    port: u16,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::UnboundedSender<Command>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (shared, tx) = (shared.clone(), tx.clone());
        tokio::spawn(async move {
            let _ = handle(stream, port, shared, tx).await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    host: String,
    origin: Option<String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err("request head too long".into());
        }
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|e| e.to_string())?.to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
    let (mut length, mut host, mut origin) = (0usize, String::new(), None);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().map_err(|_| "bad content-length")?,
                "host" => host = value.to_string(),
                "origin" => origin = Some(value.to_string()),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        return Err("request body too long".into());
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed".into());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Request { method, path, host, origin, body })
}

fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn json_response(value: serde_json::Value) -> Vec<u8> {
    response("200 OK", "application/json", value.to_string().as_bytes())
}

async fn handle(
    mut stream: TcpStream,
    port: u16,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::UnboundedSender<Command>,
) -> Result<(), String> {
    let request = read_request(&mut stream).await?;
    // Only pages from here: a web page elsewhere can't drive the glasses through this.
    let ours = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let ok_origin = request.origin.as_ref().is_none_or(|o| ours.iter().any(|h| *o == format!("http://{h}")));
    let reply = if !ours.contains(&request.host) || !ok_origin {
        response("403 Forbidden", "text/plain", b"forbidden")
    } else {
        route(&request, &shared, &tx).await
    };
    stream.write_all(&reply).await.map_err(|e| e.to_string())?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn route(request: &Request, shared: &Arc<Mutex<Shared>>, tx: &mpsc::UnboundedSender<Command>) -> Vec<u8> {
    let layout_from_body = || -> Result<Layout, String> {
        serde_json::from_slice(&request.body).map_err(|e| format!("not a layout: {e}"))
    };
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => response("200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
        ("GET", "/api/layout") => {
            let layout = shared.lock().unwrap().layout.clone();
            json_response(json!({ "layout": layout, "file": super::layout_path().display().to_string() }))
        }
        ("GET", "/api/default") => json_response(json!({ "layout": Layout::default().settle() })),
        ("GET", "/api/status") => {
            let status = shared.lock().unwrap().status.clone();
            json_response(json!(status))
        }
        ("POST", "/api/render") => {
            #[derive(serde::Deserialize)]
            struct Ask {
                layout: Layout,
                #[serde(default = "level")]
                scene: Scene,
            }
            fn level() -> Scene {
                Scene::Level
            }
            let Ask { layout, scene } = match serde_json::from_slice(&request.body) {
                Ok(ask) => ask,
                Err(e) => return json_response(json!({ "error": format!("not a layout: {e}") })),
            };
            let n = layout.tiles.len();
            match tokio::task::spawn_blocking(move || super::sim::render(&layout, scene)).await {
                Ok(Ok(render)) => json_response(json!({
                    "png": base64(&render.png),
                    "visible": (0..n).map(|i| render.visible & 1 << i != 0).collect::<Vec<_>>(),
                    "plans": render.plans,
                })),
                Ok(Err(e)) => json_response(json!({ "error": e })),
                Err(e) => json_response(json!({ "error": e.to_string() })),
            }
        }
        ("POST", "/api/apply") => {
            let layout = match layout_from_body() {
                Ok(l) => l,
                Err(e) => return json_response(json!({ "ok": false, "message": e })),
            };
            let (reply, answer) = oneshot::channel();
            if tx.send(Command::Apply(layout, reply)).is_err() {
                return json_response(json!({ "ok": false, "message": "the shell has stopped" }));
            }
            match answer.await {
                Ok(Ok(message)) => json_response(json!({ "ok": true, "message": message })),
                Ok(Err(message)) => json_response(json!({ "ok": false, "message": message })),
                Err(_) => json_response(json!({ "ok": false, "message": "no answer" })),
            }
        }
        ("POST", "/api/say") => {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let text = body.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string();
            let icon = body.get("icon").and_then(|i| i.as_str()).and_then(super::icons::category_named);
            let _ = tx.send(Command::Say(text, icon));
            json_response(json!({ "ok": true }))
        }
        ("POST", "/api/demo-route") => {
            tokio::spawn(super::demo_route(tx.clone()));
            json_response(json!({ "ok": true }))
        }
        _ => response("404 Not Found", "text/plain", b"not found"),
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
