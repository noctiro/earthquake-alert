# Earthquake Alert

面向 iOS Bark 的 Wolfx 地震预警订阅服务。服务提供一个公开网页，用户填写 Bark Key、监测位置和推送阈值；后端常驻监听 Wolfx EEW WebSocket，收到地震预警后按订阅位置估算 S 波到达时间和本地烈度，并通过 Bark 推送。

参考项目形态：<https://github.com/noctiro/earthquake-alert>、<https://eew.noctiro.moe/>

## 特性

- 公开订阅页面：`/`
- 订阅 API：`POST /api/subscribe`
- 退订 API：`DELETE /api/unsubscribe/{bark_id}`
- 统计 API：`GET /api/stats`
- 健康检查：`GET /health`
- 数据源：`wss://ws-api.wolfx.jp/all_eew`
- 存储：本地 JSON 文件，默认 `./data/subscriptions.json`
- Bark：支持官方 `https://api.day.app` 和配置化自建 Bark Server
- 部署：单二进制、systemd 或 Docker Compose

## 本地运行

```bash
cp config.example.yaml config.yaml
go run . -config config.yaml
```

打开：

```text
http://127.0.0.1:30010
```

测试 Bark 链路：

```bash
go run . -config config.yaml -test-bark YOUR_BARK_KEY
```

## 配置 Bark

默认配置使用官方 Bark：

```yaml
bark:
  server: "https://api.day.app"
  self_hosted_server: ""
  device_db_path: ""
```

如果你有自建 Bark Server，可以设置：

```yaml
bark:
  server: "https://api.day.app"
  self_hosted_server: "https://bark.example.com"
  device_db_path: "/app/bark.db"
```

用户粘贴 `https://api.day.app/{key}` 或自建 Bark URL 时，服务会保留对应 Bark 服务器地址。自建 Bark Key 会通过 `device_db_path` 指向的 Bark Server bbolt 数据库校验；只使用官方 Bark 时可留空。

## Docker 部署

```bash
cp config.example.yaml config.yaml
docker compose up -d --build
docker logs -f eew-bark
```

`docker-compose.yml` 默认只绑定：

```text
127.0.0.1:30010:30010
```

公网访问建议放在 Caddy、Nginx 或 Cloudflare Tunnel 后面。如果使用自建 Bark Key 校验，请在 `docker-compose.yml` 中挂载 Bark Server 的 `bark.db` 到 `device_db_path`。

## Caddy 反代

安装 Caddy 后：

```bash
sudo cp Caddyfile.example /etc/caddy/Caddyfile
sudo sed -i 's/eew.example.com/你的域名/g' /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

## Cloudflare Tunnel

如果不想暴露源站端口，可以使用 Tunnel：

```bash
cloudflared tunnel create earthquake-alert
cloudflared tunnel route dns earthquake-alert eew.example.com
cloudflared tunnel run earthquake-alert
```

Tunnel 的 ingress 指向：

```yaml
ingress:
  - hostname: eew.example.com
    service: http://127.0.0.1:30010
  - service: http_status:404
```

## 预警计算

```text
震中距 = haversine(订阅地, 震中)
震源距 = sqrt(震中距^2 + 震源深度^2)
```

P/S 波到达时间使用快速混合模型：

- 100 km 内：使用直达波固定速度估算，P 波默认 `6.0 km/s`，S 波默认 `3.5 km/s`。
- 100 km 以上：按震中距换算为角距，使用区域走时表插值估算 P 波和 S-P 时间。
- 深度修正：走时表以约 33 km 深度为参考，按当前震源深度和固定速度模型做轻量修正。
- 自动降级：缺少深度、距离超出走时表范围、插值结果异常时，自动降级为固定速度模型。

本地烈度是经验估算，用于推送筛选，不等同官方烈度预报。P/S 到达时间也只是基于可获取数据的快速估算，不使用完整地壳速度结构、TauP、IASP91 或区域三维速度模型。

## Bark 并发推送

收到真实 EEW 后，服务会先计算全部订阅者的距离、预计烈度和到达时间，再按优先级并发推送：

1. `critical` 优先，其次 `active`，最后 `passive`。
2. 同级别内优先推送 S 波 ETA 更短、预计烈度更高、距离更近的订阅地。
3. 按 Bark 服务器分组并发推送：官方 `api.day.app` 默认 `300` 并发，自建 Bark 默认 `1000` 并发。

建议配置：

```yaml
alert:
  fanout_concurrency: 300
  self_hosted_fanout_concurrency: 1000
  fanout_error_budget: 800
  key_failure_threshold: 3
  key_quarantine_minutes: 1440
```

官方 Bark 异常使用可能触发 IP Ban。服务对官方服务器做了错误预算和单 Key 熔断；自建 Bark Server 不套用官方错误预算。

## 投递审计

真实 EEW 事件会在 `server.audit_path` 写入持久化审计文件，默认路径为 `./data/audit`。模拟测试和历史地震测试不会写入审计。

- `EVENT-rREPORT-TYPE.jsonl`：逐条订阅投递明细。
- `EVENT-rREPORT-TYPE.summary.json`：事件汇总。

审计明细不保存完整 Bark Key，只保存掩码和 SHA-256 哈希，便于后续按用户提供的 Key 计算哈希后定位记录。

## Bark 点击跳转

默认点击通知会打开本服务的预警详情页：

```text
https://eew.example.com/alert/{token}
```

如果要强制覆盖 Bark 点击链接，可以设置：

```yaml
alert:
  click_url: "weixin://"
```

## 注意

Wolfx 是第三方聚合数据源，Bark/APNs 不是硬实时链路。这个服务适合个人或小范围预警辅助，不应作为唯一生命安全系统。
