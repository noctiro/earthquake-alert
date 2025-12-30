# 地震预警 Bark 订阅系统

基于 Rust 后端 + Cloudflare Workers 的地震预警实时推送服务，使用 GeoHash 空间索引实现高效的震度匹配。

示例: [http://eew.noctiro.moe](http://eew.noctiro.moe)

## 特性

- 🚨 **实时监控**：通过 WebSocket 接收地震预警
- 📍 **智能推送**：基于用户位置和震度阈值精准推送
- ⚡ **高性能**：GeoHash 空间索引 + sled 数据库，极速响应
- 📱 **Bark 推送**：使用 Bark 推送到装有bark的苹果设备
- 🎨 **极简界面**：现代简约设计，黑白灰配色
- 🌍 **全球 CDN**：Cloudflare Workers 加速，低延迟访问

## 技术栈

### 后端
- **语言**：Rust
- **Web 框架**：Axum
- **数据库**：sled
- **WebSocket**：tokio-tungstenite
- **HTTP 客户端**：reqwest

### 前端
- **托管**：Cloudflare Workers（纯 JS）
- **界面**：原生 HTML/CSS/JavaScript
- **地图**：cartocdn

## 快速开始

### 1. 前置要求

- [Rust](https://www.rust-lang.org/) (1.91+)
- [Node.js](https://nodejs.org/) (用于 Cloudflare Workers)
- [wrangler](https://developers.cloudflare.com/workers/wrangler/) CLI
- VPS 或服务器（用于部署后端）

### 2. 部署后端

```bash
cd backend

# 创建配置文件
cp .env.example .env
# 编辑 .env 配置你的环境

# 构建发布版本
cargo build --release

# 创建数据目录
mkdir -p data

# 运行服务器
./target/release/earthquake-alert-backend
```

### 3. 部署 Cloudflare Worker

```bash
cd worker

# 编辑 wrangler.toml，设置后端 URL
# [env.production.vars]
# BACKEND_URL = "https://your-backend-server.com"

# 登录 Cloudflare
wrangler login

# 部署到生产环境
wrangler deploy --env production
```

## 环境变量

### 后端配置

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `SERVER_HOST` | 服务器监听地址 | `0.0.0.0` |
| `SERVER_PORT` | 服务器端口 | `30010` |
| `DB_PATH` | 数据库文件路径 | `./data/earthquake.db` |
| `BARK_API_URL` | Bark API 地址 | `https://api.day.app` |
| `RUST_LOG` | 日志级别 | `earthquake_alert_backend=info` |

### Worker 配置

在 `worker/wrangler.toml` 中配置：

```toml
[vars]
BACKEND_URL = "http://your-backend-server.com:30010"
```

## 项目结构

```
earthquake-alert/
├── backend/                    # Rust 后端
│   ├── src/
│   │   ├── main.rs            # 主入口
│   │   ├── config.rs          # 配置管理
│   │   ├── db/                # 数据库层
│   │   │   ├── mod.rs
│   │   │   └── subscription_store.rs
│   │   ├── models.rs          # 数据模型
│   │   ├── routes/            # API 路由
│   │   │   ├── mod.rs
│   │   │   └── subscribe.rs
│   │   ├── services/          # 业务服务
│   │   │   ├── mod.rs
│   │   │   ├── earthquake_monitor.rs
│   │   │   └── bark_notifier.rs
│   │   └── utils/             # 工具函数
│   │       ├── geohash.rs
│   │       ├── distance.rs
│   │       └── intensity.rs
│   ├── Cargo.toml
│   ├── Dockerfile
│   └── earthquake-alert.service
├── worker/                     # Cloudflare Worker
│   ├── index.js               # Worker 入口
│   └── wrangler.toml          # Worker 配置
├── static/                     # 静态文件
│   ├── index.html
└── README.md
```

## API 接口

### 订阅地震预警

**POST** `/api/subscribe`

```json
{
  "bark_id": "your_bark_key",
  "latitude": 35.6762,
  "longitude": 139.6503,
  "min_intensity": 3
}
```

### 取消订阅

**DELETE** `/api/unsubscribe/{bark_id}`

### 获取统计

**GET** `/api/stats`

响应：
```json
{
  "success": true,
  "message": "统计成功",
  "data": {
    "total_subscriptions": 123
  }
}
```

### 健康检查

**GET** `/health`

## 开发

### 本地开发后端

```bash
cd backend

# 安装依赖
cargo build

# 运行（会自动监听文件变化）
cargo watch -x run

# 运行测试
cargo test
```

### 本地开发 Worker

```bash
cd worker

# 本地开发模式（需要先启动后端）
wrangler dev
```

## 监控和日志

### 查看后端日志

```bash
# systemd 服务
sudo journalctl -u earthquake-alert -f

# Docker
docker logs -f earthquake-alert

# 直接运行
RUST_LOG=debug ./earthquake-alert-backend
```

## 数据备份

sled 数据库文件位于 `data/` 目录，定期备份即可：

```bash
# 简单备份
tar -czf backup-$(date +%Y%m%d).tar.gz data/

# 使用 rsync 同步到远程
rsync -avz data/ backup-server:/backups/earthquake-alert/
```

## 致谢

- 数据源：[wolfx.jp](https://ws-api.wolfx.jp)
- 推送服务：[Bark](https://github.com/Finb/Bark)
