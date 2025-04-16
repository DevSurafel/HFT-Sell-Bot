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
const TARGET_TOKEN: &str = "BTCUSDT";
const COIN_AMOUNT: &str = "1020200"; // Adjust based on balance

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const WS_URL: &str = "wss://ws.bitget.com/spot/v1/stream";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Improved atomic flags for better state management
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

// Precompute the order request body and signature
static ORDER_BODY: Lazy<String> = Lazy::new(|| {
    json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    }).to_string()
});

static ORDER_SIGNATURE: Lazy<String> = Lazy::new(|| {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_BODY)
});

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

    let response = client
        .get(format!("{}{}", API_BASE_URL, BALANCE_PATH))
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

    if ORDER_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        println!("⚠️ Order execution already in progress, skipping duplicate attempt.");
        return false;
    }

    // Attempt to get cached balance or fetch if needed
    let start_time = Instant::now();
    let balance = check_balance(client, coin_symbol).await;
    let amount: f64 = COIN_AMOUNT.parse().unwrap_or(0.0);

    if let Some(avail) = balance {
        if avail < amount {
            println!(
                "⚠️ Insufficient balance: {} available, {} requested",
                avail, amount
            );
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

    println!("⏱ Preparation time: {:?}", start_time.elapsed());
    let request_start = Instant::now();

    // Execute with minimal timeout for urgency
    let response = client
        .post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &*ORDER_SIGNATURE) // Dereference Lazy<String> to get &str
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_BODY.clone())
        .timeout(Duration::from_millis(800))
        .send()
        .await;

    let elapsed = request_start.elapsed();
    println!("⏱ Sell order request latency: {:?}", elapsed);

    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            println!("📊 Response Status: {} | {}", status, text);

            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::SeqCst);
                ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
                println!(
                    "✅ SELL ORDER PLACED FOR {} at {:?}",
                    *FORMATTED_SYMBOL,
                    SystemTime::now()
                );
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

/// Polling function optimized for lower CPU usage but still fast response
async fn poll_token_status(client: Arc<Client>, tx: mpsc::Sender<String>) {
    let endpoint = format!("{}/api/spot/v1/public/products", API_BASE_URL);
    let mut backoff = 50; // Start with 50ms polling interval

    loop {
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            println!("✅ Polling stopped: Order executed successfully.");
            return;
        }

        match client
            .get(&endpoint)
            .timeout(Duration::from_millis(800))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json_resp) = resp.json::<Value>().await {
                    // Reset backoff on successful response
                    backoff = 50;

                    if json_resp["data"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .any(|item| item["symbolName"] == TARGET_TOKEN && item["status"] == "online")
                    {
                        println!("🚨 Token {} detected via polling!", TARGET_TOKEN);
                        let _ = tx.try_send(TARGET_TOKEN.to_string());
                    }
                }
            }
            Err(e) => {
                println!("Polling error: {}. Retrying with backoff...", e);
                // Increase backoff on error, cap at 500ms
                backoff = (backoff * 2).min(500);
            }
        }

        sleep(Duration::from_millis(backoff)).await;
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
                let (mut write, read) = ws_stream.split(); // Removed `mut` from `read`

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

                // Start ping task
                let write_clone = write.reunite(read).unwrap();

                let (mut ws_write, mut ws_read) = write_clone.split(); // Added `mut` to `ws_read`

                // Spawn a ping task to keep connection alive
                let ping_task = tokio::spawn(async move {
                    loop {
                        if ORDER_EXECUTED.load(Ordering::SeqCst) {
                            return;
                        }

                        if let Err(e) = ws_write.send(Message::Text(ping_msg.to_string())).await {
                            println!("❌ Failed to send ping: {}", e);
                            return;
                        }

                        sleep(Duration::from_secs(15)).await;
                    }
                });

                // Process incoming messages
                while let Some(message) = ws_read.next().await {
                    if ORDER_EXECUTED.load(Ordering::SeqCst) {
                        ping_task.abort();
                        println!("✅ WebSocket stopped: Order executed successfully.");
                        return;
                    }

                    match message {
                        Ok(msg) => {
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                if json_msg.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = json_msg
                                        .get("arg")
                                        .and_then(|a| a.get("instId"))
                                        .and_then(Value::as_str)
                                    {
                                        if inst_id == TARGET_TOKEN {
                                            println!(
                                                "🚨 TARGET TOKEN DETECTED via WebSocket: {}",
                                                inst_id
                                            );
                                            // Use try_send to avoid blocking
                                            let _ = tx.try_send(inst_id.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("WebSocket error: {}. Reconnecting...", e);
                            ping_task.abort();
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

/// Pre-calculate signature for faster order execution
async fn prepare_signature_cache(client: &Arc<Client>) {
    println!("🔐 Checking authentication and pre-warming API connections...");

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
}

/// Main async function
#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);

    // Create optimized HTTP client with connection pooling and DNS caching
    let client = Arc::new(
        ClientBuilder::new()
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to build HTTP client"),
    );

    // Warm up connections before starting
    warm_up_connections(&client).await;

    // Pre-authenticate and validate credentials
    prepare_signature_cache(&client).await;

    // Channel for communicating token detection with sufficient buffer
    let (tx, mut rx) = mpsc::channel::<String>(32);

    // High priority channel for websocket detections
    let (priority_tx, mut priority_rx) = mpsc::channel::<String>(8);

    // Spawn WebSocket listener
    let ws_tx = priority_tx.clone();
    tokio::spawn(async move {
        listen_websocket(ws_tx).await;
    });

    // Spawn polling fallback
    let client_poll = client.clone();
    let poll_tx = tx.clone();
    tokio::spawn(async move {
        poll_token_status(client_poll, poll_tx).await;
    });

    // Spawn priority order processor
    let priority_client = client.clone();
    tokio::spawn(async move {
        while let Some(coin_symbol) = priority_rx.recv().await {
            if execute_sell_order(&priority_client, &coin_symbol).await {
                println!("🎉 Bot finished: Sell order executed successfully via priority channel!");
                return;
            }
        }
    });

    // Main loop - process regular detection events
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
