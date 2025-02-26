use reqwest::Client;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::time::{sleep, Duration};

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "ZOOUSDT";
const COIN_AMOUNT: &str = "10000"; // Fixed amount to sell

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Pre-computed values
const FORMATTED_SYMBOL: &str = "ZOOUSDT_SPBL";

// Atomic flags for state management
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Generates an HMAC-SHA256 signature
#[inline]
fn sign_request(timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).expect("HMAC initialization failed");
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Executes a sell order with maximum speed
async fn execute_sell_order(client: &Arc<Client>) -> bool {
    println!("⚡ Attempting immediate sell order for {}", TARGET_TOKEN);
    
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        println!("⚠️ Order already executed, skipping.");
        return true;
    }
    
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        println!("⚠️ Order in progress, skipping attempt.");
        return false;
    }
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() // Use microseconds for finer granularity
        .to_string();
    
    // Pre-constructed body for minimum latency
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
        .timeout(Duration::from_micros(500)) // Reduced to 500 microseconds
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

/// Warm up connections to reduce initial latency
async fn warm_up_connections(client: &Arc<Client>) {
    println!("ℹ️ Warming up connections...");
    match client.get(API_BASE_URL)
        .timeout(Duration::from_millis(500))
        .send()
        .await {
            Ok(_) => println!("🔥 Connections warmed up"),
            Err(e) => println!("⚠️ Warm-up failed: {}", e),
        };
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting HFT Bot at {:?}", Instant::now());
    println!("🎯 Target: {}", TARGET_TOKEN);
    println!("💰 Sell amount: {}", COIN_AMOUNT);
    
    let client = Arc::new(Client::builder()
        .pool_max_idle_per_host(10) // Optimize connection pooling
        .tcp_nodelay(true)          // Reduce TCP latency
        .build()
        .expect("Failed to create client"));
    
    warm_up_connections(&client).await;
    
    println!("ℹ️ Starting immediate sell attempts...");
    
    loop {
        if ORDER_EXECUTED.load(Ordering::Relaxed) {
            println!("✅ Order executed, shutting down.");
            break;
        }
        
        if execute_sell_order(&client).await {
            println!("🎉 Sell executed successfully!");
            break;
        } else {
            println!("🔄 Retrying immediately...");
            // Minimal delay for CPU efficiency, still sub-millisecond
            sleep(Duration::from_micros(100)).await;
        }
    }
    
    println!("🏁 Bot completed at {:?}", Instant::now());
}
