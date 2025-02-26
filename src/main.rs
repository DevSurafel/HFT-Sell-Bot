use reqwest::Client;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "ZOOUSDT";
const COIN_AMOUNT: &str = "10000"; // Adjust based on balance

// Endpoint constants
const API_BASE_URL: &str = "https://api.bitget.com";
const BALANCE_PATH: &str = "/api/spot/v1/account/assets";
const ORDER_PATH: &str = "/api/spot/v1/trade/orders";

// Pre-compute formatted symbol
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));

// Improved atomic flags for better state management
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

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
    println!("ℹ️ Checking balance for {}", coin_symbol);
    let cache = BALANCE_CACHE.lock().await;
    if let Some(cached) = &*cache {
        if cached.timestamp.elapsed() < Duration::from_secs(1) {
            println!("ℹ️ Using cached balance: {} (age: {:?})", cached.balance, cached.timestamp.elapsed());
            return Some(cached.balance);
        }
    }
    drop(cache);
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    
    println!("ℹ️ Fetching fresh balance from API at {}", timestamp);
    let signature = sign_request(&timestamp, "GET", BALANCE_PATH, "");

    let response = client.get(format!("{}{}", API_BASE_URL, BALANCE_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .timeout(Duration::from_secs(1))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let json: Value = match resp.json().await {
                Ok(j) => {
                    println!("ℹ️ Balance API response received successfully");
                    j
                },
                Err(e) => {
                    println!("❌ Failed to parse balance response: {}", e);
                    return None
                },
            };
            
            let coin_prefix = coin_symbol.split('_').next().unwrap_or(coin_symbol);
            
            if let Some(assets) = json["data"].as_array() {
                for asset in assets {
                    if asset["coin"].as_str() == Some(coin_prefix) {
                        if let Some(avail_str) = asset["available"].as_str() {
                            if let Ok(balance) = avail_str.parse::<f64>() {
                                println!("ℹ️ Found balance: {} {}", balance, coin_prefix);
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
                println!("ℹ️ No balance found for {}", coin_prefix);
                None
            } else {
                println!("❌ Invalid balance response format");
                None
            }
        }
        Ok(resp) => {
            println!("❌ Balance API returned error: {}", resp.status());
            None
        }
        Err(e) => {
            println!("❌ Failed to fetch balance: {}", e);
            None
        }
    }
}

/// Prepares and executes a sell order with minimal latency
async fn execute_sell_order(client: &Arc<Client>, coin_symbol: &str) -> bool {
    println!("ℹ️ Attempting to execute sell order for {}", coin_symbol);
    
    if ORDER_EXECUTED.load(Ordering::SeqCst) {
        println!("⚠️ Order already executed, skipping duplicate.");
        return true;
    }
    
    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        println!("⚠️ Order execution already in progress, skipping duplicate attempt.");
        return false;
    }
    
    let start_time = Instant::now();
    let balance = check_balance(client, coin_symbol).await;
    let amount: f64 = COIN_AMOUNT.parse().unwrap_or(0.0);
    
    if let Some(avail) = balance {
        println!("ℹ️ Current balance: {} {}", avail, coin_symbol);
        if avail < amount {
            println!("⚠️ Insufficient balance: {} available, {} requested", avail, amount);
            ORDER_IN_PROGRESS.store(false, Ordering::SeqCst);
            return false;
        }
    } else {
        println!("⚠️ Could not verify balance before order placement");
    }
    
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
    println!("ℹ️ Preparing sell order: {}", body_str);
    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &body_str);
    
    println!("⏱ Preparation time: {:?}", start_time.elapsed());
    let request_start = Instant::now();
    
    let response = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .json(&body)
        .timeout(Duration::from_millis(500))
        .send()
        .await;
    
    let elapsed = request_start.elapsed();
    println!("⏱ Sell order request latency: {:?}", elapsed);
    
    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => format!("Failed to read response: {}", e),
            };
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

/// Warm up connections to reduce initial latency
async fn warm_up_connections(client: &Arc<Client>) {
    println!("ℹ️ Warming up connections...");
    match client.get(API_BASE_URL)
        .timeout(Duration::from_secs(1))
        .send()
        .await {
            Ok(_) => println!("🔥 Connections warmed up successfully"),
            Err(e) => println!("⚠️ Connection warm-up failed: {}", e),
        };
}

/// Pre-authenticate and validate credentials
async fn prepare_signature_cache(client: &Arc<Client>) {
    println!("ℹ️ Validating credentials...");
    let balance = check_balance(client, TARGET_TOKEN).await;
    match balance {
        Some(b) => println!("🔑 Credentials validated successfully - Initial balance: {}", b),
        None => println!("⚠️ Credential validation completed with no balance data"),
    };
}

/// Simple polling fallback mechanism
async fn poll_token_status(client: Arc<Client>, tx: tokio::sync::mpsc::Sender<String>) {
    println!("ℹ️ Starting balance polling...");
    loop {
        println!("🔍 Polling cycle started at {:?}", SystemTime::now());
        if let Some(balance) = check_balance(&client, TARGET_TOKEN).await {
            println!("ℹ️ Balance check: {} {}", balance, TARGET_TOKEN);
            if balance > 0.0 {
                println!("✅ Sufficient balance detected, sending sell signal");
                let _ = tx.send(TARGET_TOKEN.to_string()).await;
            } else {
                println!("ℹ️ Balance too low: {}", balance);
            }
        } else {
            println!("⚠️ Balance check failed");
        }
        println!("⏳ Waiting 500ms before next poll...");
        sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);
    println!("💰 Target sell amount: {}", COIN_AMOUNT);
    
    let client = Arc::new(Client::new());
    
    warm_up_connections(&client).await;
    prepare_signature_cache(&client).await;
    
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(32);
    
    let client_poll = client.clone();
    tokio::spawn(async move {
        poll_token_status(client_poll, tx).await;
    });
    
    println!("ℹ️ Entering main order processing loop...");
    while let Some(coin_symbol) = rx.recv().await {
        println!("ℹ️ Received sell signal for {}", coin_symbol);
        if ORDER_EXECUTED.load(Ordering::SeqCst) {
            println!("✅ Order already executed, exiting main loop.");
            break;
        }
        
        if execute_sell_order(&client, &coin_symbol).await {
            println!("🎉 Bot finished: Sell order executed successfully!");
            break;
        } else {
            println!("🔄 Retrying sell order in 100ms...");
            sleep(Duration::from_millis(100)).await;
        }
    }
    
    println!("🏁 Bot execution complete at {:?}", SystemTime::now());
}
