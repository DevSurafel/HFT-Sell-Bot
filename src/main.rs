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
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;
use simd_json::prelude::*; // Faster JSON parsing

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BTCUSDT";
const COIN_AMOUNT: &str = "10"; // Adjust based on balance

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flags for state management
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Pre-computed API paths
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Cache the balance check to avoid redundant API calls
struct BalanceCache {
    timestamp: Instant,
    balance: f64,
}

static BALANCE_CACHE: Lazy<Mutex<Option<BalanceCache>>> = Lazy::new(|| Mutex::new(None));

/// Generates an HMAC-SHA256 signature
#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Checks account balance for the token with caching
async fn check_balance(client: &Arc<Client>, coin_symbol: &str) -> Option<f64> {
    // Check cache first to avoid redundant API calls
    {
        let cache = BALANCE_CACHE.lock().await;
        if let Some(cached) = &*cache {
            // Use cached balance if less than 5 seconds old
            if cached.timestamp.elapsed() < Duration::from_secs(5) {
                return Some(cached.balance);
            }
        }
    }
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    let signature = sign_request(&timestamp, "GET", BALANCE_PATH, "");

    let response = client.get(format!("{}{}", API_BASE_URL, BALANCE_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .timeout(Duration::from_secs(3))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let json: Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => return None,
            };
            
            let coin_prefix = coin_symbol.split('_').next().unwrap_or(coin_symbol);
            
            if let Some(assets) = json["data"].as_array() {
                for asset in assets {
                    if asset["coin"].as_str() == Some(coin_prefix) {
                        if let Some(avail_str) = asset["available"].as_str() {
                            if let Ok(balance) = avail_str.parse::<f64>() {
                                // Update cache
                                let mut cache = BALANCE_CACHE.lock().await;
                                *cache = Some(BalanceCache {
                                    timestamp: Instant::now(),
                                    balance,
                                });
                                return Some(balance);
                            }
                        }
                    }
                }
            }
            None
        }
        _ => {
            eprintln!("❌ Failed to fetch balance");
            None
        }
    }
}

/// Prepares and executes a sell order with minimal latency
async fn execute_sell_order(client: &Arc<Client>, coin_symbol: &str) -> bool {
    // Prevent concurrent order execution attempts
    if ORDER_EXECUTED.load(Ordering::SeqCst) {
        println!("⚠️ Order already executed, skipping duplicate.");
        return true;
    }
    
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        println!("⚠️ Order execution already in progress, skipping duplicate attempt.");
        return false;
    }
    
    // Start timing the entire process
    let start_time = Instant::now();
    
    // Attempt to get cached balance or fetch if needed
    let balance = check_balance(client, coin_symbol).await;
    let amount: f64 = COIN_AMOUNT.parse().unwrap_or(0.0);
    
    if let Some(avail) = balance {
        if avail < amount {
            println!("⚠️ Insufficient balance: {} available, {} requested", avail, amount);
            ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
            return false;
        }
    }
    
    // Prepare order request - precompute as much as possible
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    let body = json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    });
    
    let body_str = body.to_string();
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &body_str);
    
    println!("⏱ Preparation time: {:?} µs", start_time.elapsed().as_micros());
    let request_start = Instant::now();
    
    // Execute with minimal timeout for urgency
    let response = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .json(&body)
        .timeout(Duration::from_millis(800))
        .send()
        .await;
    
    let elapsed = request_start.elapsed();
    println!("⏱ Sell order request latency: {:?} µs", elapsed.as_micros());
    
    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            println!("📊 Response Status: {} | {}", status, text);
            
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::SeqCst);
                ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
                println!("✅ SELL ORDER PLACED FOR {} at {:?}", *FORMATTED_SYMBOL, SystemTime::now());
                return true;
            } else {
                ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
                println!("❌ API ERROR: {}", text);
                return false;
            }
        }
        Err(e) => {
            ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
            println!("❌ REQUEST FAILED: {}", e);
            return false;
        }
    }
}

/// Optimized WebSocket listener
async fn listen_websocket(tx: mpsc::Sender<String>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);
    
    loop {
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            println!("✅ WebSocket stopped: Order executed successfully.");
            return;
        }
        
        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected!");
                let (mut write, mut read) = ws_stream.split();
                
                // Ping message to keep connection alive
                let ping_msg = json!({"op": "ping"});
                
                // Subscribe message
                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker", 
                        "instId": TARGET_TOKEN
                    }]
                });
                
                // Send subscribe message
                if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                    println!("❌ Failed to subscribe: {}. Reconnecting...", e);
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
                
                // Process incoming messages
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::SeqCst) {
                        println!("✅ WebSocket stopped: Order executed successfully.");
                        return;
                    }
                    
                    match message {
                        Ok(msg) => {
                            let mut json_data = msg.to_string().into_bytes();
                            if let Ok(parsed) = simd_json::from_slice::<Value>(&mut json_data) {
                                if parsed.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = parsed.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) {
                                        if inst_id == TARGET_TOKEN {
                                            println!("🚨 TARGET TOKEN DETECTED via WebSocket: {}", inst_id);
                                            let _ = tx.try_send(inst_id.to_string());
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
    
    // Create optimized HTTP client with connection pooling and DNS caching
    let client = Arc::new(ClientBuilder::new()
        .tcp_keepalive(Some(Duration::from_secs(60)))
        .timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .build()
        .expect("Failed to build HTTP client"));
    
    // Channel for communicating token detection
    let (tx, mut rx) = mpsc::channel::<String>(32);
    
    // Spawn WebSocket listener
    let ws_tx = tx.clone();
    tokio::spawn(async move {
        listen_websocket(ws_tx).await;
    });
    
    // Main loop - process detection events
    while let Some(coin_symbol) = rx.recv().await {
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            println!("✅ Order already executed, exiting main loop.");
            break;
        }
        
        if execute_sell_order(&client, &coin_symbol).await {
            println!("🎉 Bot finished: Sell order executed successfully!");
            break;
        } else {
            println!("🔄 Retrying sell order...");
            sleep(Duration::from_millis(100)).await;
        }
    }
    
    println!("🏁 Bot execution complete");
}
