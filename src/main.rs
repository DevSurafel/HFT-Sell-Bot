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

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "ZOOUSDT";
const COIN_AMOUNT: &str = "10000";

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Pre-computed values
const FORMATTED_SYMBOL: &str = "ZOOUSDT_SPBL";

// Atomic flags
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Generates HMAC-SHA256 signature
#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC init failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Executes sell order with maximum speed
async fn execute_sell_order(client: &Arc<Client>) -> bool {
    println!("⚡ Attempting immediate sell for {}", TARGET_TOKEN);
    
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        println!("⚠️ Order already executed, skipping.");
        return true;
    }
    
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        println!("⚠️ Order in progress, skipping.");
        return false;
    }
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    let body = json!({
        "symbol": FORMATTED_SYMBOL,
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
        .timeout(Duration::from_millis(50)) // Fastest reliable timeout
        .send()
        .await;
    
    let elapsed = request_start.elapsed();
    println!("⏱ Sell order latency: {:?}", elapsed);
    
    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => format!("Failed to read response: {}", e),
            };
            println!("📊 Response: {} | {}", status, text);
            
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::Relaxed);
                ORDER_IN_PROGRESS.store(false, Ordering::Relaxed);
                println!("✅ SELL ORDER EXECUTED for {} at {:?}", FORMATTED_SYMBOL, Instant::now());
                return true;
            } else {
                ORDER_IN_PROGRESS.store(false, Ordering::Relaxed);
                println!("❌ API ERROR: {}", text);
                return false;
            }
        }
        Err(e) => {
            ORDER_IN_PROGRESS.store(false, Ordering::Relaxed);
            println!("❌ REQUEST FAILED: {}", e);
            return false;
        }
    }
}

/// WebSocket listener for real-time triggers
async fn listen_websocket(tx: mpsc::Sender<String>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ WebSocket stopped: Order executed.");
            return;
        }
        
        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected!");
                let (mut write, mut read) = ws_stream.split();
                
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
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
                
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        println!("✅ WebSocket stopped: Order executed.");
                        return;
                    }
                    
                    match message {
                        Ok(msg) => {
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                if json_msg.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = json_msg.get("arg")
                                        .and_then(|a| a.get("instId"))
                                        .and_then(Value::as_str) {
                                        if inst_id == TARGET_TOKEN {
                                            println!("🚨 DETECTED {} via WebSocket!", TARGET_TOKEN);
                                            let _ = tx.try_send(TARGET_TOKEN.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("❌ WebSocket error: {}. Reconnecting...", e);
                            break;
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
    match client.get(API_BASE_URL)
        .timeout(Duration::from_millis(500))
        .send()
        .await {
            Ok(_) => println!("✅ Connections warmed up"),
            Err(e) => println!("⚠️ Warm-up failed: {}", e),
        };
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting HFT Bot at {:?}", Instant::now());
    println!("🎯 Target: {}", TARGET_TOKEN);
    println!("💰 Sell amount: {}", COIN_AMOUNT);
    
    let client = Arc::new(ClientBuilder::new()
        .pool_max_idle_per_host(20)
        .tcp_nodelay(true)
        .http2_prior_knowledge()
        .build()
        .expect("Failed to create client"));
    
    warm_up_connections(&client).await;
    
    let (tx, mut rx) = mpsc::channel::<String>(32);
    
    // Spawn WebSocket listener
    tokio::spawn(listen_websocket(tx));
    
    println!("ℹ️ Waiting for WebSocket trigger...");
    
    while let Some(_) = rx.recv().await {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Order executed, shutting down.");
            break;
        }
        
        let order_start = Instant::now();
        if execute_sell_order(&client).await {
            println!("🎉 Sell executed successfully in {:?}", order_start.elapsed());
            break;
        } else {
            println!("🔄 Retrying after minimal delay...");
            sleep(Duration::from_micros(500)).await; // 500µs retry delay
        }
    }
    
    println!("🏁 Bot completed at {:?}", Instant::now());
}
