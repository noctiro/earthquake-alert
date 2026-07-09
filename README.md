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
| `BARK_SOUND` | (空) | Bark 铃声名称，空表示使用默认 |
| `BARK_VOLUME` | `10` | Bark 推送音量 (0-10) |
| `BARK_GROUP` | `地震预警` | Bark 推送分组名 |
| `BARK_CALL` | `false` | 是否触发 Bark 通话级别推送 |
| `EEW_WEBSOCKET_URL` | `wss://ws-api.wolfx.jp/all_eew` | 地震预警 WebSocket 地址 |
| `RECONNECT_MIN_SECONDS` | `1` | 重连最小间隔秒数 |
| `RECONNECT_MAX_SECONDS` | `30` | 指数退避重连最大间隔秒数 |
| `PUSH_UPDATES` | `false` | 是否推送同一事件的后续报告 |
| `UPDATE_MIN_REPORT_GAP` | `1` | 同事件两次推送之间至少间隔的报告数 |
| `IGNORE_TRAINING` | `true` | 是否跳过演练事件 |
| `IGNORE_CANCEL` | `true` | 是否跳过取消事件 |
| `P_WAVE_KM_S` | `6.0` | P 波传播速度 (km/s) |
| `S_WAVE_KM_S` | `3.5` | S 波传播速度 (km/s) |
| `STALE_ORIGIN_SECONDS` | `600` | 发震时间超过该秒数视为过期 |
| `DEDUP_KEEP_MINUTES` | `120` | 事件去重窗口分钟数 |
| `MAX_DISTANCE_KM` | `1000` | 订阅者最大推送距离 (km)，0 表示不限制 |
| `MAX_CONCURRENT_NOTIFICATIONS` | `1000` | 并发推送上限 |
| `HTTP_POOL_SIZE` | `200` | HTTP 连接池大小 |

## 后端 API 接口

* **订阅**: `POST /api/subscribe`

```json
{
  "bark_id": "key",
  "location_name": "东京",
  "latitude": 35.6,
  "longitude": 139.6,
  "locations": [
    { "name": "东京", "latitude": 35.6, "longitude": 139.6 }
  ],
  "notify_bands": [
    { "min": 1, "max": 1, "level": "passive", "label": "低烈度" },
    { "min": 2, "max": 2, "level": "active", "label": "中等烈度" },
    { "min": 3, "max": 99, "level": "critical", "label": "高烈度" }
  ]
}
```

* **退订**: `DELETE /api/unsubscribe`

```json
{ "bark_id": "key" }
```

* **状态**: `GET /health`

* **统计**: `GET /api/stats`

## 致谢

* 数据源：[wolfx.jp](https://ws-api.wolfx.jp)
* 推送服务：[Bark](https://github.com/Finb/Bark)
