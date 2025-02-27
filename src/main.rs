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

// API Credentials
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const TARGET_TOKEN: &str = "ZOOUSDT";
const COIN_AMOUNT: &str = "20"; // Adjust based on balance

// WebSocket URL
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flag to track if the order has been executed
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);

/// Generates an HMAC-SHA256 signature with minimal overhead
#[inline(always)]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Prepares and executes a sell order via WebSocket
async fn execute_sell_order(ws_sender: &mpsc::Sender<Message>) -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }

    let start_time = Instant::now();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();

    let order_msg = json!({
        "op": "order",
        "args": [{
            "symbol": *FORMATTED_SYMBOL,
            "side": "sell",
            "orderType": "market",
            "quantity": COIN_AMOUNT,
            "force": "gtc",
            "timestamp": timestamp,
            "signature": sign_request(&timestamp, "POST", "/api/spot/v1/trade/orders", "")
        }]
    });

    if let Err(e) = ws_sender.send(Message::Text(order_msg.to_string())).await {
        println!("❌ Failed to send order via WebSocket: {}", e);
        return false;
    }

    ORDER_EXECUTED.store(true, Ordering::Release);
    println!("✅ SELL ORDER PLACED FOR {} at {:?} (latency: {:?})", *FORMATTED_SYMBOL, SystemTime::now(), start_time.elapsed());
    true
}

/// WebSocket listener for market data and order execution
async fn listen_websocket(tx: mpsc::Sender<String>, ws_sender: mpsc::Sender<Message>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);

    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected!");
                let (mut write, mut read) = ws_stream.split();

                // Subscribe to market data
                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker",
                        "instId": TARGET_TOKEN
                    }]
                });

                if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                    println!("❌ Failed to subscribe: {}. Reconnecting...", e);
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }

                // Process incoming messages
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        return;
                    }

                    match message {
                        Ok(msg) => {
                            println!("📩 Received WebSocket message: {}", msg); // Log all incoming messages

                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                println!("📊 Parsed JSON message: {:?}", json_msg); // Log parsed JSON

                                // Check if the message is a snapshot or update for the target token
                                if let Some(action) = json_msg.get("action").and_then(Value::as_str) {
                                    if action == "snapshot" || action == "update" {
                                        if let Some(inst_id) = json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) {
                                            if inst_id == TARGET_TOKEN {
                                                println!("🚨 TARGET TOKEN DETECTED via WebSocket: {}", inst_id);

                                                // Execute order immediately
                                                if !ORDER_EXECUTED.load(Ordering::Relaxed) {
                                                    let _ = execute_sell_order(&ws_sender).await;
                                                }

                                                // Notify through channel as backup
                                                let _ = tx.try_send(inst_id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("WebSocket error: {}. Reconnecting...", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ WebSocket connection failed: {}. Retrying in 1s...", e);
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Main async function
#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);

    // Channel for communicating token detection
    let (tx, mut rx) = mpsc::channel::<String>(64);

    // Channel for sending WebSocket messages
    let (ws_sender, mut ws_receiver) = mpsc::channel::<Message>(64);

    // Spawn WebSocket listener
    let ws_tx = tx.clone();
    let ws_sender_clone = ws_sender.clone();
    tokio::spawn(async move {
        listen_websocket(ws_tx, ws_sender_clone).await;
    });

    // Spawn WebSocket message handler
    tokio::spawn(async move {
        while let Some(msg) = ws_receiver.recv().await {
            println!("📩 WebSocket message: {}", msg);
        }
    });

    // Wait for token detection and execute order
    while let Some(_coin_symbol) = rx.recv().await {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Order already executed, exiting execution loop.");
            break;
        }

        if execute_sell_order(&ws_sender).await {
            println!("🎉 Bot finished: Sell order executed successfully!");
            break;
        }
    }

    println!("🏁 Bot execution complete");
}
