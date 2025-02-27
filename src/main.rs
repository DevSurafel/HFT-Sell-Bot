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
const COIN_AMOUNT: &str = "0.002"; // Adjust based on balance

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

/// Prepares and executes a sell order with minimal latency
async fn execute_sell_order(client: &Arc<Client>, coin_symbol: &str) -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) {
        return true;
    }

    if ORDER_IN_PROGRESS.load(Ordering::Relaxed) {
        return false;
    }

    if ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }

    let start_time = Instant::now();

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();

    let signature = sign_request(&timestamp, "POST", ORDER_PATH, &ORDER_TEMPLATE);

    let request = client.post(format!("{}{}", API_BASE_URL, ORDER_PATH))
        .header("Content-Type", "application/json")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", &signature)
        .header("ACCESS-TIMESTAMP", &timestamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_TEMPLATE.to_string())
        .timeout(Duration::from_millis(50)); // Reduced timeout for faster failure detection

    let response = request.send().await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            let text_future = resp.text();

            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::Release);
                ORDER_IN_PROGRESS.store(false, Ordering::Release);
                println!("✅ SELL ORDER PLACED FOR {} at {:?} (latency: {:?})", 
                         *FORMATTED_SYMBOL, SystemTime::now(), start_time.elapsed());

                tokio::spawn(async move {
                    if let Ok(text) = text_future.await {
                        println!("📊 Response details: {}", text);
                    }
                });

                return true;
            } else {
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

/// Main async function
#[tokio::main]
async fn main() {
    println!("🚀 Starting Bitget HFT Bot at {:?}", SystemTime::now());
    println!("🎯 Targeting token: {}", TARGET_TOKEN);

    let client = Arc::new(ClientBuilder::new()
        .tcp_keepalive(Some(Duration::from_secs(60)))
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(None)
        .build()
        .expect("Failed to build HTTP client"));

    let (tx, mut rx) = mpsc::channel::<String>(64);

    let order_client = client.clone();
    let execution_handle = tokio::task::spawn(async move {
        while let Some(coin_symbol) = rx.recv().await {
            if execute_sell_order(&order_client, &coin_symbol).await {
                println!("🎉 Bot finished: Sell order executed successfully!");
                break;
            }
        }
    });

    let _ = execution_handle.await;
    println!("🏁 Bot execution complete");
}
