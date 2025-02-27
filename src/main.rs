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
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use once_cell::sync::Lazy;

type HmacSha256 = Hmac<Sha256>;

// API Credentials
const API_KEY: &str = "bg_2b02e2a62b65685cee763cc916285ed3";
const SECRET_KEY: &str = "c347ccb5f4d73d8928f3c3a54258707e3bf2013400c38003fd5192d61dbeccae";
const PASSPHRASE: &str = "HFTSellNow";
const TARGET_TOKEN: &str = "ZOOUSDT";
const COIN_AMOUNT: &str = "10000";

// Pre-computed constants
static FORMATTED_SYMBOL: Lazy<String> = Lazy::new(|| format!("{}_SPBL", TARGET_TOKEN));
static ORDER_BODY_STR: Lazy<String> = Lazy::new(|| {
    json!({
        "symbol": *FORMATTED_SYMBOL,
        "side": "sell",
        "orderType": "market",
        "quantity": COIN_AMOUNT,
        "force": "gtc"
    }).to_string()
});
static HMAC_SUFFIX: Lazy<String> = Lazy::new(|| format!("POST/api/spot/v1/trade/orders{}", *ORDER_BODY_STR));

// Atomic flags
static ORDER_EXECUTED: AtomicBool = AtomicBool::new(false);
static ORDER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Optimized HTTP client
static CLIENT: Lazy<Arc<Client>> = Lazy::new(|| Arc::new(ClientBuilder::new()
    .tcp_keepalive(Some(Duration::from_secs(60)))
    .http2_prior_knowledge()
    .pool_max_idle_per_host(10)
    .build()
    .unwrap()));

fn sign_request(timestamp: &str) -> String {
    let message = format!("{}{}", timestamp, *HMAC_SUFFIX);
    let mut mac = HmacSha256::new_from_slice(SECRET_KEY.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

async fn execute_sell_order() -> bool {
    if ORDER_EXECUTED.load(Ordering::Relaxed) || 
       ORDER_IN_PROGRESS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();

    let signature = sign_request(&timestamp);

    let request = CLIENT.post("https://api.bitget.com/api/spot/v1/trade/orders")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", signature)
        .header("ACCESS-TIMESTAMP", ×tamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_BODY_STR.clone())
        .timeout(Duration::from_millis(250));

    match request.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ORDER_EXECUTED.store(true, Ordering::Release);
                true
            } else {
                false
            }
        }
        Err(_) => false
    }
}

async fn listen_websocket() {
    let (mut ws_stream, _) = connect_async(WS_URL).await.unwrap();
    let (mut write, mut read) = ws_stream.split();

    write.send(Message::Text(json!({
        "op": "subscribe",
        "args": [{"instType": "sp", "channel": "ticker", "instId": TARGET_TOKEN}]
    }).to_string())).await.unwrap();

    while let Some(Ok(msg)) = read.next().await {
        if let Ok(json_msg) = serde_json::from_str::<Value>(&msg.to_string()) {
            if json_msg.get("action") == Some(&Value::from("update")) &&
               json_msg["arg"]["instId"] == TARGET_TOKEN {
                tokio::spawn(execute_sell_order());
                break;
            }
        }
    }
}

async fn aggressive_polling() {
    let endpoint = "https://api.bitget.com/api/spot/v1/public/products";
    loop {
        if let Ok(resp) = CLIENT.get(endpoint).timeout(Duration::from_millis(100)).send().await {
            if let Ok(json) = resp.json::<Value>().await {
                if json["data"].as_array().unwrap().iter().any(|i| i["symbolName"] == TARGET_TOKEN) {
                    tokio::spawn(execute_sell_order());
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::main]
async fn main() {
    // Validate credentials and warmup
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().to_string();
    let test_sign = sign_request(&timestamp);
    
    CLIENT.post("https://api.bitget.com/api/spot/v1/trade/orders")
        .header("ACCESS-KEY", API_KEY)
        .header("ACCESS-SIGN", test_sign)
        .header("ACCESS-TIMESTAMP", ×tamp)
        .header("ACCESS-PASSPHRASE", PASSPHRASE)
        .body(ORDER_BODY_STR.clone())
        .send()
        .await
        .unwrap();

    // Launch detection mechanisms
    tokio::spawn(listen_websocket());
    tokio::spawn(aggressive_polling());

    // Monitor execution
    while !ORDER_EXECUTED.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_micros(100)).await;
    }
    println!("Order executed successfully!");
}
