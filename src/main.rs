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

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BGBUSDT";
const COIN_AMOUNT: &str = "0.2550"; // Adjust based on balance

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Atomic flags for state management with memory ordering optimized for HFT
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Pre-computed API paths
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Cache the balance with timestamp
struct BalanceCache {
    timestamp: Instant,
    balance: f64,
}

// Fast balance cache
static BALANCE_CACHE: Lazy<Mutex<Option<BalanceCache>>> = Lazy::new(|| Mutex::new(None));

// Pre-signed request template for order execution
#[derive(Clone)]
struct PreSignedTemplate {
    base_payload: String,
    signature_base: String,
}

static PRE_SIGNED_TEMPLATE: Lazy<Mutex<Option<PreSignedTemplate>>> = Lazy::new(|| Mutex::new(None));

/// Generates an HMAC-SHA256 signature with optimized memory allocation
#[inline(always)]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Pre-computes signature template for ultra-fast execution
async fn prepare_presigned_template() {
    let mut template = PRE_SIGNED_TEMPLATE.lock().await;
    
    // Prepare base payload - this part never changes
    let base_payload = json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    });
    
    let body_str = base_payload.to_string();
    
    // Create signature template (without timestamp which will be added at execution time)
    let signature_base = format!("{}POST{}", "", ORDER_PATH);
    
    *template = Some(PreSignedTemplate {
        base_payload: body_str,
        signature_base,
    });
    
    println!("✅ Pre-signed template ready");
}

/// Fast balance check with aggressive caching
async fn check_balance(client: &Arc<Client>, coin_symbol: &str) -> Option<f64> {
    // Use cached balance with extended lifetime to reduce API calls
    {
        let cache = BALANCE_CACHE.lock().await;
        if let Some(cached) = &*cache {
            // Use cached balance if less than 10 seconds old - more aggressive caching
            if cached.timestamp.elapsed() < Duration::from_secs(10) {
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

    // Optimized request with shorter timeout
    let response = client.get(format!("{}{}", API_BASE_URL, BALANCE_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .timeout(Duration::from_secs(2))
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
                                // Update cache with current balance
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

/// Ultra-low latency optimized sell order execution 
async fn execute_sell_order(client: &Arc<Client>, coin_symbol: &str) -> bool {
    // Fast path: check if order already executed with relaxed ordering
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }
    
    // Try to acquire execution lock with atomic CAS operation
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }
    
    let start_time = Instant::now();
    
    // Check if we have pre-cached balance or use default (riskier but faster)
    let mut should_proceed = true;
    let amount: f64 = COIN_AMOUNT.parse().unwrap_or(0.0);
    
    // Optional balance check - can be disabled for ultra-low latency
    // In true HFT, you might skip this check and ensure funds are available beforehand
    {
        let cache = BALANCE_CACHE.lock().await;
        if let Some(cached) = &*cache {
            if cached.balance < amount {
                should_proceed = false;
            }
        }
    }
    
    if !should_proceed {
        ORDER_IN_PROGRESS.store(false, Ordering::Release);
        return false;
    }
    
    // Ultra-fast timestamp generation
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    // Fix: Clone the template outside the lock scope
    let template_lock = PRE_SIGNED_TEMPLATE.lock().await;
    let template = match &*template_lock {
        Some(t) => t.clone(),
        None => {
            // Fallback if template not available - should never happen in production
            ORDER_IN_PROGRESS.store(false, Ordering::Release);
            return false;
        }
    };
    // Drop the lock here
    drop(template_lock);
    
    // Finalize signature with current timestamp
    let final_signature_base = format!("{}{}{}", timestamp, template.signature_base, template.base_payload);
    let signature = sign_request(&timestamp, "", "", &final_signature_base);
    
    println!("⏱ Pre-execution preparation: {:?}", start_time.elapsed());
    let request_start = Instant::now();
    
    // Execute order with ultra-low timeout and pre-populated headers
    let response = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(template.base_payload)  // Use pre-formatted payload
        .timeout(Duration::from_millis(300))  // Aggressive timeout
        .send()
        .await;
    
    let elapsed = request_start.elapsed();
    println!("⏱ Sell order execution latency: {:?}", elapsed);
    
    match response {
        Ok(resp) => {
            let status = resp.status();
            
            // Optimized response handling
            if status.is_success() {
                // Set order executed first then release lock
                ORDER_EXECUTED.store(true, Ordering::Release);
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("✅ SELL ORDER PLACED FOR {} at {:?} (latency: {:?})", *FORMATTED_SYMBOL, SystemTime::now(), elapsed);
                return true;
            } else {
                // Only get text on error for debugging
                let text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("❌ API ERROR: {}", text);
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

/// Optimized polling function
async fn poll_token_status(client: Arc<Client>, tx: mpsc::Sender<()>) {
    let endpoint = format!("{}/api/spot/v1/public/products", API_BASE_URL);
    let mut backoff = 20; // Start with 20ms polling for faster response
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }
        
        match client.get(&endpoint).timeout(Duration::from_millis(300)).send().await {
            Ok(resp) => {
                if let Ok(json_resp) = resp.json::<Value>().await {
                    backoff = 20; // Reset backoff on success
                    
                    if json_resp["data"].as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .any(|item| item["symbolName"] == TARGET_TOKEN && item["status"] == "online") {
                            println!("🚨 Token {} detected via polling!", TARGET_TOKEN);
                            // Just signal detection without string payload
                            let _ = tx.try_send(());
                        }
                }
            }
            Err(_) => {
                // Increase backoff on error, cap at 200ms
                backoff = (backoff * 2).min(200);
            }
        }
        
        // Async sleep with minimal duration
        sleep(Duration::from_millis(backoff)).await;
    }
}

/// Optimized WebSocket listener with minimal processing
async fn listen_websocket(tx: mpsc::Sender<()>) {
    println!("🔗 Connecting to WebSocket");
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }
        
        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected");
                let (mut write, mut read) = ws_stream.split();
                
                // Pre-compute messages
                let ping_msg = Message::Text(json!({"op": "ping"}).to_string());
                let subscribe_msg = Message::Text(json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker", 
                        "instId": TARGET_TOKEN
                    }]
                }).to_string());
                
                // Send subscribe message
                if let Err(_) = write.send(subscribe_msg).await {
                    sleep(Duration::from_millis(500)).await;
                    continue;
                }
                
                // Process incoming messages with minimal overhead
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        return;
                    }
                    
                    match message {
                        Ok(msg) => {
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                // Check for target token with minimal string operations
                                if json_msg.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) {
                                        if inst_id == TARGET_TOKEN {
                                            println!("🚨 TARGET TOKEN DETECTED via WebSocket!");
                                            let _ = tx.try_send(());
                                            break; // Exit this loop to trigger immediate order execution
                                        }
                                    }
                                }
                            }
                            
                            // Send ping occasionally to keep connection alive
                            if msg.is_ping() || msg.is_close() {
                                let _ = write.send(ping_msg.clone()).await;
                            }
                        }
                        Err(_) => {
                            break; // Reconnect on error
                        }
                    }
                }
            }
            Err(_) => {
                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Warm up connections and prepare HTTP client
async fn warm_up_connections(client: &Arc<Client>) {
    println!("🔥 Warming up connections...");
    
    // Prefetch DNS and establish connection pool
    let endpoints = [
        format!("{}/api/spot/v1/public/time", API_BASE_URL),
        format!("{}{}", API_BASE_URL, BALANCE_PATH),
        format!("{}{}", API_BASE_URL, ORDER_PATH),
    ];
    
    let mut handles = Vec::new();
    
    for endpoint in endpoints {
        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            let _ = client_clone.get(&endpoint).send().await;
        });
        handles.push(handle);
    }
    
    for handle in handles {
        let _ = handle.await;
    }
    
    println!("✅ Connection warmup complete");
}

/// Prepare everything for ultra-fast execution
async fn prepare_for_hft(client: &Arc<Client>) {
    println!("🔐 Preparing HFT environment...");
    
    // Check balance to ensure credentials are valid
    if let Some(balance) = check_balance(client, TARGET_TOKEN).await {
        println!("✅ Authentication successful. Available balance: {}", balance);
        
        // Pre-populate the balance cache
        let mut cache = BALANCE_CACHE.lock().await;
        *cache = Some(BalanceCache {
            timestamp: Instant::now(),
            balance,
        });
    } else {
        println!("⚠️ Could not validate authentication. Continuing anyway...");
    }
    
    // Prepare pre-signed template for ultra-fast order execution
    prepare_presigned_template().await;
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget Ultra-Low Latency HFT Bot");
    println!("🎯 Targeting token: {}", TARGET_TOKEN);
    
    // Create optimized HTTP client with aggressive timeouts for HFT
    let client = Arc::new(ClientBuilder::new()
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(20)
        // Increase connection pool size for performance
        .pool_idle_timeout(Duration::from_secs(30))
        // Disable certificate verification for lower latency (use with caution)
        .danger_accept_invalid_certs(true)
        .build()
        .expect("Failed to build HTTP client"));
    
    // Warm up connections before starting
    warm_up_connections(&client).await;
    
    // Prepare pre-signed templates and authenticate
    prepare_for_hft(&client).await;
    
    // Simple channel for token detection signal (no string payload needed)
    let (tx, mut rx) = mpsc::channel::<()>(16);
    
    // Spawn WebSocket listener with higher priority
    let ws_tx = tx.clone();
    tokio::spawn(async move {
        listen_websocket(ws_tx).await;
    });
    
    // Spawn polling fallback with lower priority
    let client_poll = client.clone();
    let poll_tx = tx.clone();
    tokio::spawn(async move {
        poll_token_status(client_poll, poll_tx).await;
    });
    
    // Direct execution channel for ultra-low latency
    let (exec_tx, mut exec_rx) = mpsc::channel::<()>(1);
    
    // Spawn execution handler on separate high-priority thread
    let exec_client = client.clone();
    tokio::spawn(async move {
        while exec_rx.recv().await.is_some() {
            // Execute order as soon as signal received
            if execute_sell_order(&exec_client, TARGET_TOKEN).await {
                println!("✅ Order executed successfully via direct channel!");
                ORDER_EXECUTED.store(true, Ordering::Release);
                return;
            }
            
            // Try again immediately if failed
            sleep(Duration::from_micros(100)).await;
            if execute_sell_order(&exec_client, TARGET_TOKEN).await {
                println!("✅ Order executed successfully via retry!");
                ORDER_EXECUTED.store(true, Ordering::Release);
                return;
            }
        }
    });
    
    // Create a timer to periodically check readiness
    let exec_tx_clone = exec_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if ORDER_EXECUTED.load(Ordering::Relaxed) {
                break;
            }
            // Ping execution system to ensure it's ready
            let _ = exec_tx_clone.try_send(());
            println!("✓ Execution channel pinged");
        }
    });
    
    // Main detection loop
    while rx.recv().await.is_some() {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Order already executed, exiting main loop");
            break;
        }
        
        // Signal execution channel immediately
        let _ = exec_tx.try_send(());
        
        // Also try directly in this thread as a backup
        if execute_sell_order(&client, TARGET_TOKEN).await {
            println!("✅ Order executed successfully via main thread!");
            ORDER_EXECUTED.store(true, Ordering::Release);
            break;
        }
    }
    
    println!("🏁 HFT Bot execution complete");
}
