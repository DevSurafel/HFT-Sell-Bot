use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use reqwest::{Client, ClientBuilder};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BGBUSDT";
const COIN_AMOUNT: &str = "0.2550";

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flags with relaxed ordering for minimal latency
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Pre-computed API paths
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Pre-prepared order payload
static ORDER_PAYLOAD: Lazy<String> = Lazy::new(|| {
    json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    }).to_string()
});

/// Generates an HMAC-SHA256 signature correctly for Bitget API
#[inline(always)]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}+{}+{}+{}", timestamp, method.to_uppercase(), path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Ultra-low latency sell order execution
async fn execute_sell_order(client: &Arc<Client>) -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }

    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }

    let start_time = Instant::now();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();

    // Correct signature generation with full payload
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_PAYLOAD);

    println!("⏱ Pre-execution preparation: {:?}", start_time.elapsed());
    let request_start = Instant::now();
    let response = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", signature)
        .header("ACCESS-TIMESTAMP", timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_PAYLOAD.clone()) // Pre-computed payload
        .timeout(Duration::from_millis(50)) // Ultra-aggressive timeout
        .send()
        .await;

    let elapsed = request_start.elapsed();
    println!("⏱ Sell order execution latency: {:?}", elapsed);

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::Release);
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("✅ SELL ORDER PLACED FOR {} (latency: {:?})", *FORMATTED_SYMBOL, elapsed);
                return true;
            } else {
                let text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("❌ API ERROR: {} - {}", status, text);
                return false;
            }
        }
        Err(e) => {
            ORDER_IN_PROGRESS.store(false, Ordering::Release);
            println!("❌ REQUEST FAILED: {}", e);
            return false;
        }
    }
}

/// Optimized WebSocket listener with minimal processing
async fn listen_websocket(client: Arc<Client>, tx: mpsc::Sender<()>) {
    println!("🔗 Connecting to WebSocket");

    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected");
                let (mut write, mut read) = ws_stream.split();

                // Pre-compute subscription message
                let subscribe_msg = Message::Text(json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker",
                        "instId": TARGET_TOKEN
                    }]
                }).to_string());

                if write.send(subscribe_msg).await.is_err() {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        return;
                    }

                    if let Ok(msg) = message {
                        if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                            if json_msg.get("action").and_then(Value::as_str) == Some("update") &&
                               json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) == Some(TARGET_TOKEN) {
                                println!("🚨 TARGET TOKEN DETECTED via WebSocket!");
                                let _ = tx.try_send(());
                                // Execute immediately in this thread for lowest latency
                                execute_sell_order(&client).await;
                                return; // Exit after detection to avoid duplicate triggers
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ WebSocket connection failed: {}. Retrying...", e);
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Warm up connections
async fn warm_up_connections(client: &Arc<Client>) {
    println!("🔥 Warming up connections...");
    let endpoints = [
        format!("{}/api/spot/v1/public/time", API_BASE_URL),
        format!("{}{}", API_BASE_URL, ORDER_PATH),
    ];

    for endpoint in endpoints {
        let _ = client.get(&endpoint).send().await;
    }
    println!("✅ Connection warmup complete");
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget Microsecond HFT Bot");
    println!("🎯 Targeting token: {}", TARGET_TOKEN);

    // Optimized HTTP client
    let client = Arc::new(ClientBuilder::new()
        .tcp_nodelay(true) // Disable Nagle's algorithm for lower latency
        .tcp_keepalive(Some(Duration::from_secs(10)))
        .pool_max_idle_per_host(50)
        .danger_accept_invalid_certs(true) // Use with caution
        .build()
        .expect("Failed to build HTTP client"));

    warm_up_connections(&client).await;

    let (tx, mut rx) = mpsc::channel::<()>(1);

    // Spawn WebSocket listener with direct execution
    let ws_client = client.clone();
    tokio::spawn(async move {
        listen_websocket(ws_client, tx).await;
    });

    // Main loop - minimal overhead
    while rx.recv().await.is_some() {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            break;
        }
        if execute_sell_order(&client).await {
            break;
        }
    }

    println!("🏁 HFT Bot execution complete");
}
