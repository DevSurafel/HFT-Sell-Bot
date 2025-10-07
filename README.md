# 🚀 Bitget HFT Trading Bot

A high-frequency trading (HFT) bot built in Rust that monitors Bitget exchange for a specific token listing and automatically executes market sell orders with minimal latency.

## 📋 Table of Contents

- [Features](#-features)
- [How It Works](#-how-it-works)
- [Technology Stack](#️-technology-stack)
- [Prerequisites](#-prerequisites)
- [Installation](#-installation)
- [Configuration](#️-configuration)
- [Usage](#-usage)
- [Architecture](#-architecture)
- [Performance Optimizations](#-performance-optimizations)
- [Security Considerations](#-security-considerations)
- [Troubleshooting](#-troubleshooting)
- [Disclaimer](#️-disclaimer)
- [License](#-license)

---

## ✨ Features

- **Real-time WebSocket monitoring** for instant token detection
- **Fallback HTTP polling** mechanism for reliability
- **Sub-second latency** for order execution
- **Balance caching** to minimize redundant API calls
- **Atomic state management** to prevent duplicate orders
- **Pre-computed signatures** and payloads for faster execution
- **Auto-reconnect** for WebSocket with exponential backoff
- **Connection warmup** and DNS pre-caching
- **Concurrent detection channels** (priority + regular)

---

## 🧠 How It Works

### 1. **Initialization Phase**
   - Loads configuration from `.env` file
   - Validates Bitget API credentials
   - Warms up HTTP connections and DNS cache
   - Pre-computes order signatures for faster execution

### 2. **Detection Phase**
   - **WebSocket Listener**: Subscribes to Bitget's real-time ticker stream for the target token
   - **HTTP Polling**: Queries `/api/spot/v1/public/products` endpoint as a fallback (50ms intervals)
   - Both methods run concurrently for maximum reliability

### 3. **Execution Phase**
   - When target token is detected:
     1. Checks account balance (with 5-second cache)
     2. Validates sufficient funds
     3. Places market sell order immediately
     4. Uses atomic flags to prevent duplicate executions

### 4. **Termination**
   - Bot automatically stops after successful order execution
   - All background tasks are cleanly terminated

---

## 🛠️ Technology Stack

| Component | Technology |
|-----------|-----------|
| **Language** | [Rust](https://www.rust-lang.org/) (Edition 2021) |
| **Async Runtime** | [Tokio](https://tokio.rs/) (multi-threaded) |
| **HTTP Client** | [Reqwest](https://docs.rs/reqwest) |
| **WebSocket** | [Tokio-Tungstenite](https://docs.rs/tokio-tungstenite) |
| **Serialization** | [Serde](https://serde.rs/) & [serde_json](https://docs.rs/serde_json) |
| **Cryptography** | HMAC-SHA256 ([hmac](https://docs.rs/hmac), [sha2](https://docs.rs/sha2)) |
| **Environment Variables** | [dotenv](https://docs.rs/dotenv) |

---

## 📦 Prerequisites

- **Rust** 1.70 or higher ([Install Rust](https://rustup.rs/))
- **Bitget Account** with API access enabled
- **API Credentials**:
  - API Key
  - Secret Key
  - Passphrase

---

## 🔧 Installation

1. **Clone the repository:**
   ```bash
   git clone https://github.com/yourusername/bitget-hft-bot.git
   cd bitget-hft-bot
   ```

2. **Install dependencies:**
   ```bash
   cargo build --release
   ```

3. **Create `.env` file:**
   ```bash
   cp .env.example .env
   ```

4. **Configure your credentials** (see [Configuration](#️-configuration))

---

## ⚙️ Configuration

Create a `.env` file in the project root with the following variables:

```env
# Bitget API Credentials
BITGET_API_KEY=your_api_key_here
BITGET_SECRET_KEY=your_secret_key_here
BITGET_PASSPHRASE=your_passphrase_here

# Trading Parameters
TARGET_TOKEN=PAWSUSDT
COIN_AMOUNT=1020200
```

### Configuration Parameters

| Variable | Description | Example |
|----------|-------------|---------|
| `BITGET_API_KEY` | Your Bitget API key | `bg_xxxxx...` |
| `BITGET_SECRET_KEY` | Your Bitget secret key | `c347ccb5f4d7...` |
| `BITGET_PASSPHRASE` | Your API passphrase | `MyPassphrase123` |
| `TARGET_TOKEN` | Token symbol to monitor | `PAWSUSDT` |
| `COIN_AMOUNT` | Amount to sell (in token units) | `1020200` |

---

## 🚀 Usage

### Run in Debug Mode
```bash
cargo run
```

### Run Optimized Release Build
```bash
cargo run --release
```

### Example Output
```
🚀 Starting Bitget HFT Bot at 2025-01-15 14:33:21
🎯 Targeting token: PAWSUSDT
🔥 Warming up connections and DNS cache...
✅ Connection warmup complete
🔐 Checking authentication and pre-warming API connections...
✅ Authentication successful. Available balance: 1500000.0
🔗 Connecting to WebSocket: wss://ws.bitget.com/spot/v1/stream
✅ WebSocket connected!
🚨 TARGET TOKEN DETECTED via WebSocket: PAWSUSDT
⏱ Preparation time: 2.3ms
⏱ Sell order request latency: 156ms
📊 Response Status: 200 | {"code":"00000","msg":"success",...}
✅ SELL ORDER PLACED FOR PAWSUSDT_SPBL at SystemTime { ... }
🎉 Bot finished: Sell order executed successfully via priority channel!
```

---

## 🏗️ Architecture

### Core Components

```
┌─────────────────────────────────────────────────────────────┐
│                         Main Thread                          │
│  - Initializes HTTP client with connection pooling          │
│  - Loads environment variables                               │
│  - Spawns concurrent tasks                                   │
└────────────────┬────────────────────────────┬────────────────┘
                 │                            │
        ┌────────▼────────┐          ┌────────▼────────┐
        │  WebSocket      │          │  HTTP Polling   │
        │  Listener       │          │  Fallback       │
        │  (Priority)     │          │  (Regular)      │
        └────────┬────────┘          └────────┬────────┘
                 │                            │
                 └──────────┬─────────────────┘
                            │
                   ┌────────▼────────┐
                   │  Order Executor │
                   │  (Atomic State) │
                   └─────────────────┘
```

### Thread Safety

- **Atomic Flags**: `ORDER_EXECUTED` and `ORDER_IN_PROGRESS` prevent race conditions
- **Lazy Static**: Pre-computed values loaded once at startup
- **Mutex-Protected Cache**: Balance cache uses `tokio::sync::Mutex`
- **Channel Communication**: `mpsc` channels for inter-task messaging

---

## ⚡ Performance Optimizations

1. **Pre-computed Signatures**: Order signatures calculated at startup
2. **Connection Pooling**: HTTP client maintains persistent connections
3. **DNS Caching**: Pre-warms DNS resolution for API endpoints
4. **Balance Caching**: 5-second cache reduces redundant API calls
5. **Concurrent Detection**: WebSocket + polling run in parallel
6. **Lazy Evaluation**: Configuration loaded once using `once_cell::Lazy`
7. **Minimal Timeouts**: 800ms for order execution (balance between speed and reliability)

### Measured Latency
- **Preparation Time**: ~2-5ms
- **Order Execution**: ~100-200ms (network dependent)
- **Total Response Time**: ~150-250ms from detection to order placement

---

## 🔒 Security Considerations

### ⚠️ CRITICAL: Protect Your API Credentials

1. **Never commit `.env` to version control**
   - Add `.env` to `.gitignore`
   - Use `.env.example` as a template

2. **API Permission Settings**
   - Enable only **Trade** permission (no withdrawals)
   - Whitelist your IP address in Bitget settings
   - Use a sub-account with limited funds

3. **Environment Security**
   - Run on trusted infrastructure only
   - Use encrypted connections (HTTPS/WSS)
   - Monitor for unauthorized access

4. **Code Review**
   - Audit changes before deploying
   - Test in paper trading mode first
   - Set reasonable `COIN_AMOUNT` limits

---

## 🐛 Troubleshooting

### Bot Won't Start

**Problem**: `BITGET_API_KEY must be set in .env file`

**Solution**: Ensure `.env` file exists and contains all required variables

---

### WebSocket Connection Failed

**Problem**: `WebSocket connection failed: ... Retrying in 1s...`

**Solution**: 
- Check internet connection
- Verify Bitget API is accessible
- Check for firewall/proxy blocking WSS connections

---

### Authentication Failed

**Problem**: `⚠️ Could not validate authentication. Please check your API credentials.`

**Solution**:
- Verify API credentials are correct
- Check API permissions (Trade must be enabled)
- Ensure IP whitelist is configured if enabled

---

### Insufficient Balance

**Problem**: `⚠️ Insufficient balance: 100 available, 1020200 requested`

**Solution**: Adjust `COIN_AMOUNT` in `.env` to match your available balance

---

### Order Already Executed

**Problem**: `⚠️ Order already executed, skipping duplicate.`

**Solution**: This is expected behavior - restart bot to allow new orders

---

## ⚖️ Disclaimer

> **USE AT YOUR OWN RISK**
> 
> This bot executes **real market orders** on your Bitget account. Cryptocurrency trading involves substantial risk of loss. The authors are not responsible for any financial losses incurred.
>
> **Before using with real funds:**
> - Thoroughly test in a sandbox/paper trading environment
> - Start with small amounts
> - Understand market volatility and slippage
> - Never invest more than you can afford to lose

---

## 📄 License

MIT License

Copyright (c) 2025

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

---

## 🤝 Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/improvement`)
3. Commit your changes (`git commit -am 'Add new feature'`)
4. Push to the branch (`git push origin feature/improvement`)
5. Open a Pull Request

---

## 📞 Support

For questions or issues:
- Open an [Issue](https://github.com/yourusername/bitget-hft-bot/issues)
- Read the [Bitget API Documentation](https://bitgetlimited.github.io/apidoc/en/spot/)

---

**Made with ⚡ by [Your Name]**
