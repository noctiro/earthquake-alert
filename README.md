# 灾害预警 Bark 订阅系统

通过 Bark 接收地震、气象、海啸和台风信息。服务提供网页订阅界面，也可以直接调用 HTTP API。

示例：<http://alert.noctiro.moe>

## 功能

- 接收 Wolfx 和 FAN Studio 提供的灾害信息
- 支持地震预警、地震速报、气象预警、海啸预警和台风信息
- 每个 Bark 订阅可以配置最多 3 个监测地点
- 可按灾种、信息来源、预计烈度、震级、严重度和距离设置通知条件
- 地震通知显示监测点预计烈度、距离以及 P 波和 S 波到达时间
- 地震预警会按监测点的实际 S 波剩余时间每秒更新，直到震波到达
- 不同灾种使用独立的 Bark 标题和正文排版，不显示内部渠道、事件 ID 等开发字段
- 通知可打开详情页查看灾害信息和本次命中的订阅条件
- 服务重启后会继续处理尚未完成的订阅确认和通知

地震波到达时间由起震时间、距离、深度和配置的波速估算。震级不改变传播时间，但会影响监测点的预计烈度；预计烈度未命中订阅规则时不会发送通知。

## 部署

需要 Rust `1.97` 或更高版本。

```bash
cp .env.example .env
cargo build --release
./target/release/disaster-alert
```

### 通过 GitHub Actions 编译

向仓库推送提交，或在 GitHub 的 **Actions → CI → Run workflow** 手动运行工作流。
成功后，从该次运行的 Artifacts 下载
`disaster-alert-linux-x86_64-<commit>`。其中包含 Linux x86_64 Release 二进制和
SHA-256 校验文件。

在 VPS 上解压并安装：

```bash
sha256sum -c disaster-alert-linux-x86_64.tar.gz.sha256
tar -xzf disaster-alert-linux-x86_64.tar.gz
sudo install -m 0755 disaster-alert /usr/local/bin/disaster-alert
```

该产物在 Debian Bookworm 容器中编译，适用于 glibc 2.34 或更高版本的 x86_64
Linux VPS，不需要在 VPS 上安装 Rust。ARM64 VPS 和 glibc 2.28 的 CentOS Linux 8
不能使用这个产物。

完整的 GitHub Pages 前端、Cloudflare Tunnel API 域名、VPS 后端、systemd、更新和备份步骤见
[配置指南](deploy/MANUAL.md)。

启动前必须设置：

- `ALERT_DETAIL_BASE_URL`：手机可以访问的服务地址。生产环境必须使用 HTTPS
- `ALERT_SIGNING_KEY`：用于保护通知详情链接的私钥
- `DATA_ENCRYPTION_KEY`：用于加密数据库内 Bark Key 的 32 字节私钥

对外提供服务前，实例部署者还应阅读“使用与部署责任”，并主动设置 `INSTANCE_TERMS_ACCEPTED=true`。未设置该变量不会阻止服务启动。

生成签名私钥和数据库加密私钥（需要分别生成两次）：

```bash
openssl rand 32 | base64 | tr '+/' '-_' | tr -d '=\n'
```

将结果写入 `.env`：

```dotenv
INSTANCE_TERMS_ACCEPTED=true
ALLOWED_ORIGINS=https://alerts.example.com
ALERT_DETAIL_BASE_URL=https://api.alerts.example.com
ALERT_SIGNING_KEY=生成的私钥
DATA_ENCRYPTION_KEY=另一次生成的私钥
```

两域名部署时，`ALLOWED_ORIGINS` 填 GitHub Pages 前端地址，`ALERT_DETAIL_BASE_URL` 填后端 API
公网地址；前端 API 地址由 GitHub Actions 里的 `DISASTER_API_BASE` 仓库变量生成到 `web/config.js`。

不要提交真实的 `.env`，也不要将任一私钥输出到日志。修改签名私钥后，之前发送的详情链接会失效；丢失或修改数据库加密私钥后，已有 Bark Key 无法恢复。

`INSTANCE_TERMS_ACCEPTED` 默认为 `false`。未确认时服务仍会启动，API、实时数据源和通知能力也不会因此自动关闭，但订阅页面每次加载都会显示实例配置提醒。网页中的“继续查看”只关闭当前弹窗，不会代表部署者确认，也不会修改服务端配置。只有实例运营者阅读并接受下方责任声明后，才应在部署环境中将该变量设为 `true`。

服务默认监听 `0.0.0.0:30010`。浏览器访问 `http://服务器地址:30010` 即可打开订阅页面。生产环境建议监听 `127.0.0.1`，再通过反向代理提供 HTTPS。

数据库保存在 `DB_PATH` 目录。部署时必须持久化整个目录，同一目录只能由一个服务实例使用。

通知详情 URL 包含访问凭据。反向代理、CDN、WAF、APM 和分析系统不应记录 `/incidents/` 路径的完整 URL。

## 配置

应用会读取当前工作目录下的 `.env`。进程环境变量优先于 `.env`；完整示例见 [.env.example](.env.example)。

### 服务

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `INSTANCE_TERMS_ACCEPTED` | `false` | 部署者是否已明确接受本 README 的“使用与部署责任”；不属于许可证明或功能安全开关 |
| `SERVER_HOST` | `0.0.0.0` | 监听地址 |
| `SERVER_PORT` | `30010` | 服务端口 |
| `ALLOWED_ORIGINS` | 空 | 允许访问 API 的前端 Origin，多个值用逗号分隔 |
| `DB_PATH` | `./data/disaster-alert.fjall` | 数据库目录 |
| `DATA_ENCRYPTION_KEY` | 必填 | 32 字节、无填充的 URL-safe Base64 私钥，用于 AES-256-GCM 加密 Bark Key 所在记录 |
| `SHUTDOWN_TIMEOUT_SECONDS` | `15` | 服务关闭时的最长等待时间，范围 `1..=300` 秒 |

### Bark

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `BARK_SOUND` | 空 | Bark 铃声名称，空表示使用默认铃声 |
| `BARK_VOLUME` | `10` | 通知音量，范围 `0..=10` |
| `BARK_GROUP` | `灾害预警` | Bark 通知分组名 |
| `BARK_CALL` | `true` | 是否为非静默灾害通知启用 Bark 通话级提醒 |
| `ALERT_DETAIL_BASE_URL` | 必填 | 通知详情页的公网根地址 |
| `ALERT_SIGNING_KEY` | 必填 | 32 字节、无填充的 URL-safe Base64 私钥 |

本项目固定使用 Bark 官方服务 `https://api.day.app`，不会接受客户端提供的其他 Bark Server。

### 灾害数据

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `WOLFX_WEBSOCKET_URL` | `wss://ws-api.wolfx.jp/all_eew` | Wolfx 地震预警地址 |
| `FANSTUDIO_WEBSOCKET_URL` | `wss://ws.fanstudio.tech/all` | FAN Studio 聚合数据地址 |
| `RECONNECT_MIN_SECONDS` | `1` | 数据源断开后的最小重连间隔 |
| `RECONNECT_MAX_SECONDS` | `30` | 数据源断开后的最大重连间隔 |
| `PUSH_UPDATES` | `false` | 是否推送同一事件的后续报告 |
| `UPDATE_MIN_REPORT_GAP` | `1` | 后续报告至少间隔多少个报告编号才再次推送 |
| `IGNORE_TRAINING` | `true` | 是否忽略演练信息 |
| `IGNORE_CANCEL` | `false` | 是否忽略取消或解除信息，通常应保持 `false` |
| `STALE_ORIGIN_SECONDS` | `600` | 忽略起震时间超过该秒数的地震预警 |
| `P_WAVE_KM_S` | `6.0` | P 波估算速度，单位 km/s |
| `S_WAVE_KM_S` | `3.5` | S 波估算速度，单位 km/s |

其余环境变量用于数据保留、Bark 并发和反向地理编码，保持 [.env.example](.env.example) 中的默认值即可。

## 使用与部署责任

本仓库提供可独立部署的软件源代码。项目维护者不运营、控制或认可第三方使用本项目搭建的实时灾害信息、订阅、通知或预警服务。

将 `INSTANCE_TERMS_ACCEPTED=true` 写入部署环境，表示实例运营者明确确认：

- 启用实时数据或向他人提供服务前，应自行核查部署地、服务对象所在地及数据来源所在地适用的法律法规，并取得主管部门和数据提供方要求的许可或授权
- 实例运营者对数据接入、内容展示、通知发送、个人信息处理、数据保存和服务对象范围承担责任；自部署不等于获准向社会发布预警
- 本项目及其处理的信息可能延迟、缺失或误报，不属于官方预警，也不应作为唯一的灾害预警、安全决策或应急行动依据

该环境变量只记录部署者的明确确认，不能替代法律评估、行政许可、数据授权或个人信息处理依据，也不能证明某项部署当然合法。若部署者不能确认上述事项，应保持默认值 `false`，并停止对外提供实时功能。

## 从旧版 sled 迁移

只需要迁移旧版订阅时，先编译迁移工具：

```bash
cargo build --release --features migration --bin disaster-alert-migrate
```

停止旧服务后执行：

```bash
./target/release/disaster-alert-migrate \
  ./data/disaster-alert.db/ \
  ./data/disaster-alert.fjall
```

迁移完成后，将 `DB_PATH` 指向新目录。迁移工具只迁移订阅，不迁移旧通知任务和历史记录。迁移期间不要同时运行新旧服务。

## API

大多数用户可以直接使用网页，无需手动调用 API。

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/api/subscribe` | 创建或覆盖订阅 |
| `DELETE` | `/api/unsubscribe` | 删除订阅 |
| `GET` | `/api/bark-urls` | 获取可用的 Bark 服务地址 |
| `GET` | `/api/subscription-options` | 获取灾种、来源和默认规则 |
| `GET` | `/api/reverse-geocode` | 根据坐标查询行政区 |
| `GET` | `/api/status` | 获取订阅总数、数据源和后台任务状态 |
| `GET` | `/health` | 健康检查 |

接口统一返回：

```json
{
  "success": true,
  "message": "操作成功",
  "data": {}
}
```

### 创建订阅

```http
POST /api/subscribe
Content-Type: application/json
```

```json
{
  "destination": {
    "type": "bark",
    "base_url": "https://api.day.app",
    "device_key": "yourBarkKey"
  },
  "targets": [
    {
      "label": "上海家中",
      "point": { "latitude": 31.2304, "longitude": 121.4737 },
      "region": { "province": "上海市", "city": "上海市", "district": "浦东新区" }
    }
  ],
  "alerts": [
    {
      "category": "earthquake_warning",
      "sources": { "mode": "all" },
      "estimated_intensity_bands": [
        { "min": 1, "max": 1, "interruption_level": "passive" },
        { "min": 2, "max": 2, "interruption_level": "active" },
        { "min": 3, "max": 7, "interruption_level": "critical" }
      ]
    }
  ]
}
```

每个订阅支持 1 到 3 个监测地点。`base_url` 必须是 `https://api.day.app`，Bark Key 只能包含字母和数字，最长 64 个字符。

可用灾种规则：

| 灾种 | `category` | 主要条件 |
| --- | --- | --- |
| 地震预警 | `earthquake_warning` | `estimated_intensity_bands`，预计烈度范围 `0..=7` |
| 地震速报 | `earthquake_report` | `min_magnitude`，最低震级 `0..=10` |
| 气象预警 | `weather_warning` | `min_severity` 和 `fallback_radius_km` |
| 海啸预警 | `tsunami` | `min_severity`，范围 `1..=4` |
| 台风信息 | `typhoon` | `max_center_distance_km`，范围 `1..=3000` km |

`sources` 支持两种形式：

```json
{ "mode": "all" }
```

```json
{ "mode": "include", "ids": ["fanstudio.cenc"] }
```

提交订阅后，服务会发送 Bark 测试通知。收到成功响应表示订阅已经生效；返回 `202` 表示 Bark 暂时不可用，服务会在后台继续确认。

### 删除订阅

```http
DELETE /api/unsubscribe
Content-Type: application/json
```

```json
{
  "destination": {
    "type": "bark",
    "base_url": "https://api.day.app",
    "device_key": "yourBarkKey"
  }
}
```

订阅身份由 Bark 服务地址和 Bark Key 共同确定。

## 隐私

服务会保存监测地点和通知规则；Bark Key 所在的订阅及待确认记录使用 AES-256-GCM 加密后落库。部署和开发时请遵守以下约束：

- 不要提交真实 `.env`、数据库、Bark Key 或签名私钥
- 不要在日志、截图、Issue 或测试数据中使用真实 Bark Key 和用户位置
- 不要记录通知详情 URL 的完整路径
- 统计接口只返回聚合数量
- 系统不提供通过 Bark Key 查询订阅内容的接口

## 开发

```bash
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
```

更多开发约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 致谢

- 数据源：[wolfx.jp](https://ws-api.wolfx.jp)
- 数据源：[FAN Studio](https://api.fanstudio.tech/doc/ws-api/#home)
- 推送服务：[Bark](https://github.com/Finb/Bark)
