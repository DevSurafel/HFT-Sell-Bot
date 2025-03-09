use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio::time::Duration;
use once_cell::sync::Lazy;
use http::Request;
use hyper::{Body, Client as HyperClient, client::HttpConnector};
use hyper_tls::HttpsConnector;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BGBUSDT";
const COIN_AMOUNT: &str = "0.2562";

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";

// Pre-computed constants
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Pre-computed order body as raw bytes
static ORDER_BODY: Lazy<Vec<u8>> = Lazy::new(|| {
    format!(
        r#"{{"symbol":"{}","side":"sell","orderType":"market","quantity":"{}","force":"gtc"}}"#,
        *FORMATTED_SYMBOL, COIN_AMOUNT
    ).into_bytes()
});

// Cached signature (refreshed periodically)
static ORDER_SIGNATURE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &[u8]) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, String::from_utf8_lossy(body));
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

async fn check_balance(client: &Arc<HyperClient<HttpsConnector<HttpConnector>>>) -> Option<f64> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().to_string();
    let signature = sign_request(&timestamp, "GET", BALANCE_PATH, &[]);

    let request = Request::builder()
        .method("GET")
        .uri(format!("{}{}", API_BASE_URL, BALANCE_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(Body::empty())
        .unwrap();

    let response = client.request(request).await;
    match response {
        Ok(resp) => {
            let body = hyper::body::to_bytes(resp.into_body()).await.ok()?;
            let json: Value = serde_json::from_slice(&body).ok()?;
            let coin_prefix = TARGET_TOKEN.split('_').next().unwrap_or(TARGET_TOKEN);
            if let Some(assets) = json["data"].as_array() {
                for asset in assets {
                    if asset["coin"].as_str() == Some(coin_prefix) {
                        return asset["available"].as_str()?.parse::<f64>().ok();
                    }
                }
            }
            None
        }
        Err(_) => None,
    }
}

async fn execute_sell_order(client: &Arc<HyperClient<HttpsConnector<HttpConnector>>>) -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) || ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return false;
    }

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros().to_string();
    let signature = {
        let mut sig = ORDER_SIGNATURE.lock().await;
        if sig.is_empty() || Instant::now().elapsed().as_secs() > 5 {
            *sig = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_BODY);
        }
        sig.clone()
    };

    let start = Instant::now();
    let request = Request::builder()
        .method("POST")
        .uri(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(Body::from(ORDER_BODY.clone()))
        .unwrap();

    let response = client.request(request).await;
    let latency = start.elapsed();

    ORDER_IN_PROGRESS.store(false, Ordering::Relaxed);
    match response {
        Ok(resp) if resp.status().is_success() => {
            ORDER_EXECUTED.store(true, Ordering::Relaxed);
            println!("✅ Sell order executed in {:?}", latency);
            true
        }
        Ok(resp) => {
            let body = hyper::body::to_bytes(resp.into_body()).await.unwrap_or_default();
            println!("❌ Sell order failed: {:?}", body);
            false
        }
        Err(e) => {
            println!("❌ Request error: {}", e);
            false
        }
    }
}

async fn listen_websocket(tx: mpsc::Sender<String>) {
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            break;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                let (mut write, mut read) = ws_stream.split();
                let subscribe_msg = json!({"op": "subscribe", "args": [{"instType": "sp", "channel": "ticker", "instId": TARGET_TOKEN}]}).to_string();
                write.send(Message::Text(subscribe_msg)).await.ok();

                while let Some(msg) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(msg) = msg {
                        if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                            if json_msg.get("action").and_then(Value::as_str) == Some("update") &&
                               json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) == Some(TARGET_TOKEN) {
                                tx.send(TARGET_TOKEN.to_string()).await.ok();
                            }
                        }
                    }
                }
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn warm_up_connections(client: &Arc<HyperClient<HttpsConnector<HttpConnector>>>) {
    let endpoints = [
        format!("{}/api/spot/v1/public/time", API_BASE_URL),
        format!("{}{}", API_BASE_URL, BALANCE_PATH),
        format!("{}{}", API_BASE_URL, ORDER_PATH),
    ];
    for endpoint in endpoints {
        let req = Request::get(endpoint).body(Body::empty()).unwrap();
        let _ = client.request(req).await;
    }
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting HFT Bot at {:?}", SystemTime::now());
    let connector = HttpsConnector::new();
    let client = Arc::new(HyperClient::builder().build::<_, Body>(connector));

    warm_up_connections(&client).await;
    if let Some(balance) = check_balance(&client).await {
        println!("✅ Balance: {}", balance);
        if balance < COIN_AMOUNT.parse::<f64>().unwrap() {
            println!("❌ Insufficient balance");
            return;
        }
    }

    let (tx, mut rx) = mpsc::channel::<String>(32);
    tokio::spawn(listen_websocket(tx));

    while let Some(_) = rx.recv().await {
        if execute_sell_order(&client).await {
            println!("🎉 Sell order completed successfully!");
            break;
        }
    }
    println!("🏁 Bot execution complete");
}
