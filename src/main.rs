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
use tokio::sync::Mutex;
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BTCUSDT";
const COIN_AMOUNT: &str = "10"; 

// Endpoints
const API_BASE_URL: &str = "https://api.bitget.com";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Precomputed Symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic Flags
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);

/// Generate HMAC-SHA256 signature
#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC init failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Ultra-low-latency sell execution
async fn execute_sell_order(client: &Arc<Client>) -> bool {
    if ORDER_EXECUTED.load(Ordering::SeqCst) {
        return true;
    }

    let start_time = Instant::now();

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros()
        .to_string(); // Using microseconds instead of milliseconds

    let body = json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    });

    let body_str = body.to_string();
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &body_str);

    let request_start = Instant::now();

    let response = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .json(&body)
        .timeout(Duration::from_millis(500)) // Ensure no long waits
        .send()
        .await;

    let elapsed = request_start.elapsed();
    println!("⏱ Sell order execution time: {:?}", elapsed);

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::SeqCst);
                println!("✅ SELL ORDER PLACED at {:?}", start_time.elapsed());
                return true;
            } else {
                println!("❌ API ERROR: {}", resp.text().await.unwrap_or_default());
                return false;
            }
        }
        Err(e) => {
            println!("❌ REQUEST FAILED: {}", e);
            return false;
        }
    }
}

/// WebSocket Listener for ultra-fast execution
async fn listen_websocket(client: Arc<Client>) {
    println!("🔗 Connecting to WebSocket: {}", API_BASE_URL);

    let (ws_stream, _) = connect_async(WS_URL).await.expect("WebSocket connection failed");
    let (mut write, mut read) = ws_stream.split();

    let subscribe_message = json!({
        "op": "subscribe",
        "args": [{
            "channel": "ticker",
            "instId": *FORMATTED_SYMBOL
        }]
    });

    write.send(Message::Text(subscribe_message.to_string())).await.expect("Failed to subscribe");

    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                if text.contains("trade") {
                    println!("🚀 Trade detected: Executing Sell Order!");
                    execute_sell_order(&client).await;
                    return;
                }
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() {
    let client = Arc::new(ClientBuilder::new().timeout(Duration::from_millis(300)).build().unwrap());

    tokio::spawn(listen_websocket(client.clone()));

    loop {
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            break;
        }
    }

    println!("🏁 HFT Execution Complete");
}
