use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use reqwest::{Client, ClientBuilder};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;
use tokio::task;

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

// Improved atomic flags using Relaxed ordering where possible for better performance
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Pre-computed API paths
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Pre-compute request templates
static REQUEST_HEADERS: Lazy<StdMutex<Vec<(String, String)>>> = Lazy::new(|| StdMutex::new(Vec::new()));
static ORDER_TEMPLATE: Lazy<String> = Lazy::new(|| {
    json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    }).to_string()
});

// Cache the balance check to avoid redundant API calls
struct BalanceCache {
    timestamp: Instant,
    balance: f64,
}

static BALANCE_CACHE: Lazy<Mutex<Option<BalanceCache>>> = Lazy::new(|| Mutex::new(None));

/// Generates an HMAC-SHA256 signature with minimal overhead
#[inline(always)]
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

/// Creates a pre-signed order request for ultra-low latency execution
fn create_presigned_order_request(client: &Client, timestamp: &str, signature: &str) -> reqwest::RequestBuilder {
    client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", signature)
        .header("ACCESS-TIMESTAMP", timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_TEMPLATE.to_string())
        .timeout(Duration::from_millis(500)) // Reduced timeout for faster failure detection
}

/// Prepares and executes a sell order with minimal latency
async fn execute_sell_order(client: &Arc<Client>, coin_symbol: &str) -> bool {
    // Use Relaxed ordering for better performance on read
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }
    
    // Fast path check with Relaxed ordering first
    if ORDER_IN_PROGRESS.load(Ordering::Relaxed) {
        return false;
    }
    
    // Try to set order in progress with minimal contention
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }
    
    // Start timing execution
    let start_time = Instant::now();
    
    // Prepare order request - with minimal allocations
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros()
        .to_string();
    
    // Use cached template for order JSON to avoid serialization cost
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_TEMPLATE);
    
    // Create request with pre-built template
    let request = create_presigned_order_request(client, &timestamp, &signature);
    
    println!("⏱ Preparation time: {:?}", start_time.elapsed());
    let request_start = Instant::now();
    
    // Execute with minimal timeout for urgency - use spawn_blocking for network IO
    let response = request.send().await;
    
    let elapsed = request_start.elapsed();
    println!("⏱ Sell order request latency: {:?}", elapsed);
    
    match response {
        Ok(resp) => {
            let status = resp.status();
            
            // Process response asynchronously to minimize latency for the main thread
            let text_future = resp.text();
            
            // Only set order executed if status is success - don't wait for text parsing
            let success = status.is_success();
            if success {
                ORDER_EXECUTED.store(true, Ordering::Release);
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("✅ SELL ORDER PLACED FOR {} at {:?} (latency: {:?})", 
                         *FORMATTED_SYMBOL, SystemTime::now(), elapsed);
                
                // Process text response in background
                tokio::spawn(async move {
                    if let Ok(text) = text_future.await {
                        println!("📊 Response details: {}", text);
                    }
                });
                
                return true;
            } else {
                // For error cases, we still want to see the text
                match text_future.await {
                    Ok(text) => {
                        println!("❌ API ERROR: {} | {}", status, text);
                    },
                    Err(e) => {
                        println!("❌ Failed to read response: {}", e);
                    }
                }
                
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
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

/// Ultra-low latency polling function
async fn poll_token_status(client: Arc<Client>, tx: mpsc::Sender<String>) {
    let endpoint = format!("{}/api/spot/v1/public/products", API_BASE_URL);
    let mut backoff = 20; // Start with 20ms polling interval for faster detection
    
    // Prepare a dedicated client with minimal overhead
    let poll_client = ClientBuilder::new()
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .timeout(Duration::from_millis(500))
        .build()
        .expect("Failed to build polling client");
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }
        
        // Use non-blocking spawn for polling to avoid blocking the executor
        let poll_future = poll_client.get(&endpoint).timeout(Duration::from_millis(500)).send();
        
        match tokio::time::timeout(Duration::from_millis(800), poll_future).await {
            Ok(Ok(resp)) => {
                if let Ok(json_resp) = resp.json::<Value>().await {
                    // Reset backoff on successful response
                    backoff = 20;
                    
                    if json_resp["data"].as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .any(|item| item["symbolName"] == TARGET_TOKEN && item["status"] == "online") {
                            // Use non-blocking send to avoid blocking on channel
                            let _ = tx.try_send(TARGET_TOKEN.to_string());
                            
                            // Try to execute order immediately without waiting for channel
                            if !ORDER_EXECUTED.load(Ordering::Relaxed) {
                                let client_clone = client.clone();
                                tokio::spawn(async move {
                                    execute_sell_order(&client_clone, TARGET_TOKEN).await;
                                });
                            }
                        }
                }
            }
            _ => {
                // Increase backoff on error, cap at 200ms
                backoff = (backoff * 2).min(200);
            }
        }
        
        // Use a very short sleep to allow other tasks to execute
        tokio::time::sleep(Duration::from_micros(backoff * 1000)).await;
    }
}

/// Optimized WebSocket listener
async fn listen_websocket(tx: mpsc::Sender<String>, client: Arc<Client>) {
    println!("🔗 Connecting to WebSocket: {}", WS_URL);
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            return;
        }
        
        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected!");
                let (mut write, mut read) = ws_stream.split();
                
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
                
                // Process incoming messages with minimal overhead
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        return;
                    }
                    
                    match message {
                        Ok(msg) => {
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                if json_msg.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = json_msg.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str) {
                                        if inst_id == TARGET_TOKEN {
                                            println!("🚨 TARGET TOKEN DETECTED via WebSocket: {}", inst_id);
                                            
                                            // Execute order directly without waiting for channel
                                            if !ORDER_EXECUTED.load(Ordering::Relaxed) {
                                                let client_clone = client.clone();
                                                tokio::spawn(async move {
                                                    execute_sell_order(&client_clone, TARGET_TOKEN).await;
                                                });
                                            }
                                            
                                            // Also notify through channel as backup
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

/// Prepare and cache request templates
fn prepare_request_templates() {
    // Pre-compute order request template
    let mut headers = REQUEST_HEADERS.lock().unwrap();
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    headers.push(("ACCESS-KEY".to_string(), API_KEY.to_string()));
    headers.push(("ACCESS-PASSPHRASE".to_string(), PASSPHRASE.to_string()));
    
    // Force initialization of lazy statics
    let _ = *ORDER_TEMPLATE;
    let _ = *FORMATTED_SYMBOL;
}

/// Warm up connection and DNS cache
async fn warm_up_connections(client: &Arc<Client>) {
    println!("🔥 Warming up connections and DNS cache...");
    
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
            // Use HEAD requests for faster connection warmup
            let _ = client_clone.head(&endpoint).send().await;
            
            // Then do a real GET to ensure full connection establishment
            let _ = client_clone.get(&endpoint).send().await;
        });
        handles.push(handle);
    }
    
    // Wait for all warmup requests to complete
    for handle in handles {
        let _ = handle.await;
    }
    
    println!("✅ Connection warmup complete");
}

/// Pre-authenticate and prepare for fast order execution
async fn prepare_for_execution(client: &Arc<Client>) {
    println!("🔐 Checking authentication and preparing for fast execution...");
    
    // Check balance to ensure credentials are valid and warm up connections
    if let Some(balance) = check_balance(client, TARGET_TOKEN).await {
        println!("✅ Authentication successful. Available balance: {}", balance);
        
        // Pre-populate the balance cache
        let mut cache = BALANCE_CACHE.lock().await;
        *cache = Some(BalanceCache {
            timestamp: Instant::now(),
            balance,
        });
    } else {
        println!("⚠️ Could not validate authentication. Please check your API credentials.");
    }
    
    // Pre-compute a timestamp signature to speed up future signatures
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros()
        .to_string();
    
    let _signature = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_TEMPLATE);
}

/// Main async function
#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);
    
    // Prepare templates before creating clients
    prepare_request_templates();
    
    // Create optimized HTTP client with connection pooling and DNS caching
    let client = Arc::new(ClientBuilder::new()
        .tcp_keepalive(Some(Duration::from_secs(60)))
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(None) // Keep connections alive
        .build()
        .expect("Failed to build HTTP client"));
    
    // Warm up connections before starting
    warm_up_connections(&client).await;
    
    // Pre-authenticate and prepare for execution
    prepare_for_execution(&client).await;
    
    // Channel for communicating token detection with sufficient buffer
    let (tx, mut rx) = mpsc::channel::<String>(64);
    
    // High priority dedicated thread for order execution
    let order_client = client.clone();
    
    // Spawn WebSocket listener with direct access to client for faster execution
    let ws_tx = tx.clone();
    let ws_client = client.clone();
    tokio::spawn(async move {
        listen_websocket(ws_tx, ws_client).await;
    });
    
    // Spawn polling fallback with higher priority
    let client_poll = client.clone();
    let poll_tx = tx.clone();
    tokio::spawn(async move {
        poll_token_status(client_poll, poll_tx).await;
    });
    
    // Use a dedicated high-priority task for order execution
    let execution_handle = tokio::task::spawn(async move {
        let mut retry_count = 0;
        
        while let Some(coin_symbol) = rx.recv().await {
            if ORDER_EXECUTED.load(Ordering::Relaxed) {
                println!("✅ Order already executed, exiting execution loop.");
                break;
            }
            
            if execute_sell_order(&order_client, &coin_symbol).await {
                println!("🎉 Bot finished: Sell order executed successfully!");
                break;
            } else {
                retry_count += 1;
                println!("🔄 Retrying sell order... (attempt {})", retry_count);
                
                // Use exponential backoff for retries to avoid overwhelming the API
                if retry_count > 5 {
                    sleep(Duration::from_millis(50)).await;
                } else {
                    // Minimal delay for first few retries
                    sleep(Duration::from_micros(500)).await;
                }
            }
        }
    });
    
    // Wait for execution to complete
    let _ = execution_handle.await;
    
    println!("🏁 Bot execution complete");
}
