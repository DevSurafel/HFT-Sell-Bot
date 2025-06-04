# Bitget HFT Sell Bot

This is a high-frequency trading bot designed to detect when a specific token (`PAWSUSDT`) becomes available for trading on Bitget and immediately places a market sell order for a predefined quantity. It uses both WebSocket and HTTP polling to ensure fast reaction time.

---

## 🚀 Features

- ✅ Real-time detection using Bitget WebSocket
- ✅ Fallback polling mechanism for reliability
- ✅ Optimized latency for market sell order
- ✅ Cached balance checks to minimize redundant API calls
- ✅ Atomic state control to avoid duplicate orders
- ✅ Pre-computed signature and payload to boost execution speed
- ✅ Auto reconnect for WebSocket and error-resilient polling

---

## ⚙️ Tech Stack

- [Rust](https://www.rust-lang.org/)
- [Tokio](https://docs.rs/tokio)
- [Tungstenite](https://docs.rs/tokio-tungstenite)
- [Reqwest](https://docs.rs/reqwest)
- [Serde](https://docs.rs/serde)
- [HMAC / SHA256](https://docs.rs/hmac)

---

## 🔑 Requirements

- Rust 1.70+
- Bitget API credentials:
  - API Key
  - Secret Key
  - Passphrase

---

## 🛠️ Configuration

Update the following constants in the source file:

```rust
const API_KEY: &str = "your_api_key";
const SECRET_KEY: &str = "your_secret_key";
const PASSPHRASE: &str = "your_passphrase";
const TARGET_TOKEN: &str = "PAWSUSDT";
const COIN_AMOUNT: &str = "1020200";
```

---

## 🧠 How It Works

1. **Warm-Up Phase:**
   - Prepares HTTP client, prefetches DNS, and validates API credentials.

2. **Detection Phase:**
   - Starts WebSocket listener to detect if `PAWSUSDT` goes online.
   - Polls Bitget's `/products` endpoint as a backup.

3. **Execution Phase:**
   - Once the target token is online, checks your balance.
   - Places a **market sell order** immediately if sufficient balance exists.

---

## 🧪 Example Output

```
🚀 Starting Bitget HFT Bot at 2025-06-04 14:33:21
🎯 Targeting token: PAWSUSDT
✅ WebSocket connected!
🚨 TARGET TOKEN DETECTED via WebSocket: PAWSUSDT
📊 Response Status: 200 | {"code":"00000", "msg":"success", ...}
✅ SELL ORDER PLACED FOR PAWSUSDT_SPBL
🎉 Bot finished: Sell order executed successfully!
🏁 Bot execution complete
```

---

## 📦 Dependencies

Add the following to your `Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.20"
futures-util = "0.3"
reqwest = { version = "0.11", features = ["json", "stream"] }
hmac = "0.12"
sha2 = "0.10"
base64 = "0.21"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
once_cell = "1.17"
```

---

## 📋 Notes

- WebSocket and polling channels run concurrently.
- Uses atomic flags to prevent multiple sell orders.
- Caches balance for 5 seconds to reduce API spam.

---

## 🛡️ Disclaimer

> Use at your own risk. This bot executes real orders on your Bitget account. Ensure your API credentials are secured and that the bot is thoroughly tested in paper/sandbox environments before using with real funds.

---

## 📄 License

MIT License

```

Let me know if you want the same for `Cargo.toml`, `.env` usage, or a multi-token version.
