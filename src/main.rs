use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use hyper::{Body, Client as HyperClient, Request};
use hyper_tls::HttpsConnector;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BGBUSDT";
const COIN_AMOUNT: &str = "0.256";

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flags
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_PREPARED: AtomicBool = AtomicBool::new(false);

// Precomputed order body
static ORDER_BODY: Lazy<String> = Lazy::new(|| {
    json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    }).to_string()
});

/// Generates HMAC-SHA256 signature
#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC init failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Pre-validates balance during warmup
async fn pre_validate_balance(client: &Arc<HyperClient<HttpsConnector<hyper::client::HttpConnector>>>) -> bool {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().to_string();
    let signature = sign_request(&timestamp, "GET", BALANCE_PATH, "");

    let req = Request::builder()
        .method("GET")
        .uri(format!("{}{}", API_BASE_URL, BALANCE_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(Body::empty())
        .unwrap();

    let start = Instant::now();
    match client.request(req).await {
        Ok(resp) => {
            let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap_or_default();
            let coin_prefix = TARGET_TOKEN.split('_').next().unwrap_or(TARGET_TOKEN);
            if let Some(assets) = json["data"].as_array() {
                for asset in assets {
                    if asset["coin"].as_str() == Some(coin_prefix) {
                        if let Some(avail) = asset["available"].as_str() {
                            if let Ok(balance) = avail.parse::<f64>() {
                                let amount = COIN_AMOUNT.parse::<f64>().unwrap_or(0.0);
                                println!("⏱ Balance check latency: {:?}", start.elapsed());
                                return balance >= amount;
                            }
                        }
                    }
                }
            }
            false
        }
        Err(e) => {
            println!("❌ Balance check failed: {}", e);
            false
        }
    }
}

/// Executes sell order with minimal latency
async fn execute_sell_order(client: &Arc<HyperClient<HttpsConnector<hyper::client::HttpConnector>>>) -> bool {
    if ORDER_EXECUTED.load(Ordering::SeqCst) {
        println!("⚠️ Order already executed");
        return true;
    }

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().to_string();
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_BODY);

    let req = Request::builder()
        .method("POST")
        .uri(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(Body::from(ORDER_BODY.clone()))
        .unwrap();

    let start = Instant::now();
    match client.request(req).await {
        Ok(resp) => {
            let elapsed = start.elapsed();
            println!("⏱ Sell order latency: {:?}", elapsed);
            let status = resp.status();
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::SeqCst);
                println!("✅ Sell order executed at {:?}", SystemTime::now());
                true
            } else {
                let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
                println!("❌ Order failed: {} - {:?}", status, body);
                false
            }
        }
        Err(e) => {
            println!("❌ Request error: {}", e);
            false
        }
    }
}

/// Optimized WebSocket listener
async fn listen_websocket(client: Arc<HyperClient<HttpsConnector<hyper::client::HttpConnector>>>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);

    loop {
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            println!("✅ WebSocket stopped");
            return;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected");
                let (mut write, mut read) = ws_stream.split();

                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{"instType": "sp", "channel": "ticker", "instId": TARGET_TOKEN}]
                });
                write.send(Message::Text(subscribe_msg.to_string())).await.unwrap();

                while let Some(message) = read.next().await {
                    let start = Instant::now();
                    if ORDER_EXECUTED.load(Ordering::SeqCst) {
                        break;
                    }

                    if let Ok(msg) = message {
                        let text = msg.to_string();
                        // Fast check for target token in raw message
                        if text.contains(TARGET_TOKEN) && text.contains("\"action\":\"update\"") {
                            println!("🚨 Token detected in {:?} via WebSocket", start.elapsed());
                            if execute_sell_order(&client).await {
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ WebSocket error: {}. Reconnecting...", e);
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Warm up connections
async fn warm_up_connections(client: &Arc<HyperClient<HttpsConnector<hyper::client::HttpConnector>>>) {
    let endpoints = [
        format!("{}/api/spot/v1/public/time", API_BASE_URL),
        format!("{}{}", API_BASE_URL, ORDER_PATH),
    ];

    for endpoint in endpoints {
        let req = Request::builder()
            .method("GET")
            .uri(endpoint)
            .body(Body::empty())
            .unwrap();
        let _ = client.request(req).await;
    }
    println!("✅ Warmup complete");
}

/// Main function
#[tokio::main]
async fn main() {
    println!("🚀 Starting HFT Bot at {:?}", SystemTime::now());

    // Hyper client with HTTPS
    let https = HttpsConnector::new();
    let client = Arc::new(HyperClient::builder().build::<_, hyper::Body>(https));

    // Warm up and validate balance
    warm_up_connections(&client).await;
    if !pre_validate_balance(&client).await {
        println!("❌ Insufficient balance or validation failed");
        return;
    }

    // Spawn WebSocket listener
    let ws_client = client.clone();
    tokio::spawn(async move {
        listen_websocket(ws_client).await;
    });

    // Keep main thread alive
    while !ORDER_EXECUTED.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(100)).await;
    }
    println!("🏁 Bot complete");
}
