use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use reqwest::{Client, ClientBuilder};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant, Duration as StdDuration};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;
use std::thread;
use std::collections::HashMap;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "BGBUSDT";
const COIN_AMOUNT: &str = "0.2562"; // Adjust based on balance

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

// Pre-computed HTTP headers and request templates
static PRE_COMPUTED_HEADERS: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static PRE_COMPUTED_ORDER_BODY: Lazy<String> = Lazy::new(|| {
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

/// Generates an HMAC-SHA256 signature with inline optimization
#[inline(always)]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Checks account balance for the token with aggressive caching
async fn check_balance(client: &Arc<Client>, coin_symbol: &str) -> Option<f64> {
    // Check cache first to avoid redundant API calls
    {
        let cache = BALANCE_CACHE.lock().await;
        if let Some(cached) = &*cache {
            // Use cached balance if less than 10 seconds old
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

/// Pre-computes and caches request headers and signatures for ultra-low latency
async fn pre_compute_order_request(client: &Arc<Client>) -> Result<(), Box<dyn std::error::Error>> {
    println!("⚡ Pre-computing order request components for microsecond execution...");
    
    // Pre-compute timestamp and signature
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &PRE_COMPUTED_ORDER_BODY);
    
    let mut headers = PRE_COMPUTED_HEADERS.lock().await;
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("ACCESS-KEY".to_string(), API_KEY.to_string());
    headers.insert("ACCESS-SIGN".to_string(), signature);
    headers.insert("ACCESS-TIMESTAMP".to_string(), timestamp);
    headers.insert("ACCESS-PASSPHRASE".to_string(), PASSPHRASE.to_string());
    
    // Perform a test request to warm up connection pool and DNS cache
    let test_req = client
        .get(format!("{}/api/spot/v1/public/time", API_BASE_URL))
        .send()
        .await?;
    
    if test_req.status().is_success() {
        println!("✅ Pre-computed request components and warmed connections");
        Ok(())
    } else {
        Err("Failed to warm up API connection".into())
    }
}

/// Ultra-optimized sell order execution with microsecond precision
async fn execute_sell_order_microsecond(client: &Arc<Client>, coin_symbol: &str) -> bool {
    // Prevent concurrent order execution attempts with memory ordering optimized for speed
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }

    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }

    // Use dedicated high-priority thread for API request
    let client_clone = client.clone();
    
    // Start precise timing for microsecond measurement
    let start_nano = std::time::Instant::now();
    
    // Create a fresh timestamp for this exact execution moment
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros()
        .to_string();
    
    // Compute signature with the fresh timestamp for this specific execution
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &PRE_COMPUTED_ORDER_BODY);
    
    // Use thread priority boost for the sell order execution
    let execution_handle = tokio::task::spawn_blocking(move || {
        // Set current thread to highest priority if on Linux
        #[cfg(target_os = "linux")]
        unsafe {
            libc::pthread_setschedprio(libc::pthread_self(), 99);
        }
        
        // Create a blocking client for more direct network control
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        
        runtime.block_on(async {
            // Execute with absolute minimal timeout for urgency - 200ms
            client_clone
                .post(format!("{}{}", API_BASE_URL, ORDER_PATH))
                .header("Content-Type", "application/json")
                .header("ACCESS-KEY", API_KEY)
                .header("ACCESS-SIGN", &signature)
                .header("ACCESS-TIMESTAMP", &timestamp)
                .header("ACCESS-PASSPHRASE", PASSPHRASE)
                .body(PRE_COMPUTED_ORDER_BODY.to_string())
                .timeout(Duration::from_millis(200))
                .send()
                .await
        })
    });
    
    // Wait for the high-priority execution to complete
    let response = match execution_handle.await {
        Ok(result) => result,
        Err(e) => {
            println!("❌ Thread execution failed: {}", e);
            ORDER_IN_PROGRESS.store(false, Ordering::Release);
            return false;
        }
    };
    
    // Calculate elapsed time in microseconds for precise measurement
    let elapsed_micros = start_nano.elapsed().as_micros();
    println!("⏱️ Sell order execution time: {} microseconds", elapsed_micros);

    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(_) => "Unknown error".to_string()
            };
            
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::Release);
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("✅ SELL ORDER PLACED IN {} MICROSECONDS!", elapsed_micros);
                println!("✅ Response: {}", text);
                return true;
            } else {
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

/// Zero-delay WebSocket listener with direct execution on detection
async fn listen_websocket_zero_delay(client: Arc<Client>) {
    println!("🔗 Connecting to WebSocket with zero-delay execution path: {}", WS_URL);

    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ WebSocket stopped: Order executed successfully.");
            return;
        }

        match connect_async(WS_URL).await {
            Ok((ws_stream, _)) => {
                println!("✅ WebSocket connected with zero-delay configuration!");
                let (mut write, mut read) = ws_stream.split();

                // Subscribe message with higher priority
                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": "sp",
                        "channel": "ticker",
                        "instId": TARGET_TOKEN
                    }]
                });

                // Send subscribe message with priority
                if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                    println!("❌ Failed to subscribe: {}. Reconnecting...", e);
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Process incoming messages with zero delay execution path
                let client_clone = client.clone();
                while let Some(message) = read.next().await {
                    if ORDER_EXECUTED.load(Ordering::Relaxed) {
                        println!("✅ WebSocket stopped: Order executed successfully.");
                        return;
                    }

                    match message {
                        Ok(msg) => {
                            // Parse with minimal overhead
                            if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
                                if json_msg.get("action").and_then(Value::as_str) == Some("update") {
                                    if let Some(inst_id) = json_msg
                                        .get("arg")
                                        .and_then(|a| a.get("instId"))
                                        .and_then(Value::as_str)
                                    {
                                        if inst_id == TARGET_TOKEN {
                                            println!("🚨 TARGET TOKEN DETECTED via WebSocket: {}", inst_id);
                                            
                                            // Execute order directly without channel overhead
                                            let detection_time = Instant::now();
                                            tokio::spawn(async move {
                                                if execute_sell_order_microsecond(&client_clone, inst_id).await {
                                                    println!("⚡ EXECUTION LATENCY: {:?}", detection_time.elapsed());
                                                }
                                            });
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
                println!("❌ WebSocket connection failed: {}. Retrying in 500ms...", e);
                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// High-frequency polling with aggressive timeouts and direct execution path
async fn poll_token_status_high_freq(client: Arc<Client>) {
    let endpoint = format!("{}/api/spot/v1/public/products", API_BASE_URL);
    let polling_interval = Duration::from_millis(20); // 20ms polling for high frequency

    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Polling stopped: Order executed successfully.");
            return;
        }

        let poll_start = Instant::now();
        match client
            .get(&endpoint)
            .timeout(Duration::from_millis(150))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json_resp) = resp.json::<Value>().await {
                    if json_resp["data"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .any(|item| item["symbolName"] == TARGET_TOKEN && item["status"] == "online")
                    {
                        println!("🚨 Token {} detected via polling in {:?}!", TARGET_TOKEN, poll_start.elapsed());
                        
                        // Direct execution path without channel overhead
                        if execute_sell_order_microsecond(&client, TARGET_TOKEN).await {
                            println!("⚡ Direct execution from polling completed!");
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                if !e.is_timeout() {
                    println!("Polling error: {}. Continuing...", e);
                }
            }
        }

        // Adaptive sleep to maintain polling frequency
        let elapsed = poll_start.elapsed();
        if elapsed < polling_interval {
            sleep(polling_interval - elapsed).await;
        }
    }
}

/// Sets up CPU affinity and priority for the main thread
fn optimize_thread_priority() {
    // Set thread priority to maximum if on Unix-like systems
    #[cfg(target_os = "linux")]
    unsafe {
        // Set the process priority to real-time
        libc::setpriority(libc::PRIO_PROCESS, 0, -20);
        
        // Lock memory to prevent swapping
        libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
        
        // Set CPU affinity to core 0 (typically has lower latency)
        let mut cpu_set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(0, &mut cpu_set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpu_set);
        
        // Set scheduler to SCHED_FIFO (real-time scheduler)
        let mut param: libc::sched_param = std::mem::zeroed();
        param.sched_priority = 99; // Max priority
        libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
    }
    
    println!("🚀 Thread priorities optimized for microsecond execution");
}

/// Pre-generates order templates and warms up system
async fn pre_warm_system(client: &Arc<Client>) {
    println!("🔥 Pre-warming system for microsecond execution...");
    
    // Pre-compute order request components
    if let Err(e) = pre_compute_order_request(client).await {
        println!("⚠️ Failed to pre-compute order request: {}", e);
    }
    
    // Pre-fetch balance
    if let Some(balance) = check_balance(client, TARGET_TOKEN).await {
        println!("✅ Balance pre-fetched: {} {}", balance, TARGET_TOKEN);
    }
    
    // Perform rapid connection tests to warm up network stack
    let mut handles = Vec::new();
    for _ in 0..5 {
        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            let _ = client_clone
                .get(format!("{}/api/spot/v1/public/time", API_BASE_URL))
                .timeout(Duration::from_millis(500))
                .send()
                .await;
        });
        handles.push(handle);
    }
    
    for handle in handles {
        let _ = handle.await;
    }
    
    println!("✅ System pre-warming complete - ready for microsecond execution");
}

/// Main async function
#[tokio::main]
async fn main() {
    println!("⚡ Starting Bitget Microsecond HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);
    
    // Apply system optimizations for lowest latency
    optimize_thread_priority();
    
    // Create optimized HTTP client with aggressive timeouts
    let client = Arc::new(
        ClientBuilder::new()
            .tcp_keepalive(Some(StdDuration::from_secs(60)))
            .pool_max_idle_per_host(20)
            .tcp_nodelay(true) // Disable Nagle's algorithm for lowest latency
            .min_tls_version(reqwest::tls::Version::TLS_1_2) // Faster handshakes
            .http2_keep_alive_interval(Some(StdDuration::from_secs(5)))
            .http2_keep_alive_timeout(Some(StdDuration::from_secs(20)))
            .build()
            .expect("Failed to build HTTP client"),
    );
    
    // Pre-warm system for microsecond execution
    pre_warm_system(&client).await;
    
    // Launch WebSocket direct execution path
    let ws_client = client.clone();
    tokio::spawn(async move {
        listen_websocket_zero_delay(ws_client).await;
    });
    
    // Launch high-frequency polling backup path
    let poll_client = client.clone();
    tokio::spawn(async move {
        poll_token_status_high_freq(poll_client).await;
    });
    
    // Keep main thread alive until order execution completes
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Main thread: Order execution confirmed. Exiting...");
            // Allow a moment for all logs to flush
            sleep(Duration::from_millis(100)).await;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    
    println!("🏁 HFT Bot execution complete - Microsecond execution achieved");
}
