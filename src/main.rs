use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials (DO NOT HARDCODE IN PRODUCTION - USE ENVIRONMENT VARIABLES OR SECURE VAULTS)
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "PAWSUSDT";
const COIN_AMOUNT: &str = "0.002"; // Adjust based on balance
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flag to track if the order has been executed
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

async fn execute_sell_order(ws_sender: &mpsc::Sender<Message>) -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        println!("ℹ️ Order already executed, skipping...");
        return true;
    }

    let start_time = Instant::now();
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().to_string();

    // Sign the request
    let signature = sign_request(&timestamp, "POST", "/api/spot/v1/trade/orders", "");

    let order_msg = json!({
        "op": "order",
        "args": [{
            "symbol": *FORMATTED_SYMBOL,
            "side": "sell",
            "orderType": "market",
            "quantity": COIN_AMOUNT,
            "force": "gtc",
            "timestamp": timestamp,
            "signature": signature,
            "api_key": API_KEY,
            "passphrase": PASSPHRASE
        }]
    });

    if let Err(e) = ws_sender.send(Message::Text(order_msg.to_string())).await {
        println!("❌ Failed to send order: {}", e);
        return false;
    }

    ORDER_EXECUTED.store(true, Ordering::Release);
    println!("✅ SELL ORDER PLACED FOR {} (latency: {:?})", *FORMATTED_SYMBOL, start_time.elapsed());
    true
}

async fn check_and_execute(ws_sender: mpsc::Sender<Message>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);

    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected!");
                let (mut write, mut read) = ws_stream.split();

                // Subscribe to the target token's ticker channel
                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker",
                        "instId": TARGET_TOKEN
                    }]
                });

                if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                    println!("❌ Failed to subscribe: {}. Reconnecting in 100µs...", e);
                    sleep(Duration::from_micros(100)).await; // Reduced from 1s to 100µs
                    continue;
                }

                // Listen for WebSocket messages
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        return;
                    }

                    match message {
                        Ok(msg) => {
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                if let Some(action) = json_msg.get("action").and_then(Value::as_str) {
                                    if (action == "snapshot" || action == "update") &&
                                        json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) == Some(TARGET_TOKEN) {
                                        println!("🚨 Token {} detected as listed!", TARGET_TOKEN);
                                        execute_sell_order(&ws_sender).await;
                                        return; // Exit after successful execution
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("❌ WebSocket error: {}. Reconnecting in 100µs...", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ WebSocket connection failed: {}. Retrying in 100µs...", e);
                sleep(Duration::from_micros(100)).await; // Reduced from 1s to 100µs
            }
        }
    }
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);

    let (ws_sender, mut ws_receiver) = mpsc::channel::<Message>(64);

    // Spawn the WebSocket listener and executor
    tokio::spawn(check_and_execute(ws_sender.clone()));

    // Handle WebSocket messages
    while let Some(msg) = ws_receiver.recv().await {
        println!("📩 WebSocket message sent: {}", msg);
    }

    println!("🏁 Bot execution complete");
}
