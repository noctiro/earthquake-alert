# 地震预警 Bark 订阅系统

基于 Rust 后端 + Cloudflare Workers 的地震预警实时推送服务。使用 GeoHash 空间索引实现匹配，通过 Bark App 实时推送。

示例: [http://eew.noctiro.moe](http://eew.noctiro.moe)

## 技术栈

* **后端**: Rust, Axum, sled (DB), tokio-tungstenite (WS)
* **前端**: Cloudflare Workers, 原生 JS/HTML, CartoCDN (地图)

## 部署

### 1. 后端部署 (Rust)

需要 Rust 环境和一台服务器。

```bash
cd backend

# 配置环境
cp .env.example .env
# 编辑 .env 修改 SERVER_PORT 或 BARK_API_URL 等

# 构建与运行
cargo build --release
mkdir -p data
./target/release/earthquake-alert-backend

```

### 2. 前端部署 (Cloudflare Workers)

需要 Node.js 和 Wrangler CLI。

```bash
cd worker

# 编辑 wrangler.toml 配置后端地址
# [vars]
# BACKEND_URL = "http://your-backend-ip:30010"

# 部署
wrangler deploy --env production

```

## 配置说明

### 后端环境变量 (.env)

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `SERVER_HOST` | `0.0.0.0` | 监听地址 |
| `SERVER_PORT` | `30010` | 服务端口 |
| `DB_PATH` | `./data/earthquake.db` | 数据库路径 |
| `BARK_API_URL` | `https://api.day.app` | Bark 服务器地址 |

## 后端 API 接口

* **订阅**: `POST /api/subscribe`
```json
{ "bark_id": "key", "latitude": 35.6, "longitude": 139.6, "min_intensity": 3 }

```


* **退订**: `DELETE /api/unsubscribe/{bark_id}`
* **状态**: `GET /health`
* **统计**: `GET /api/stats`

## 致谢

* 数据源：[wolfx.jp](https://ws-api.wolfx.jp)
* 推送服务：[Bark](https://github.com/Finb/Bark)
