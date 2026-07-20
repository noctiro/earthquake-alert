# GitHub Pages + Cloudflare Tunnel 部署指南

本文是本项目唯一保留的部署说明文档，适用于：

- 前端页面部署到 GitHub Pages。
- 后端服务和 Fjall 数据库存放在 VPS。
- 后端只监听 `127.0.0.1:30010`。
- Cloudflare Tunnel 把公网 API 域名转发到 VPS 本机端口。
- Bark 推送固定直接使用 `https://api.day.app`，不自建 Bark Server。

示例域名：

```text
前端页面: https://alerts.example.com
后端 API: https://api.alerts.example.com
```

执行前把所有 `alerts.example.com` 和 `api.alerts.example.com` 替换为你的真实域名。

## 1. 部署结构

```text
浏览器
  |
  | https://alerts.example.com
  v
GitHub Pages 静态前端
  |
  | https://api.alerts.example.com/api/...
  v
Cloudflare Tunnel
  |
  | http://127.0.0.1:30010
  v
VPS 上的 disaster-alert 后端 + Fjall 数据库
```

这个方案里：

- GitHub Pages 只托管 `web/` 静态文件。
- VPS 上的后端只监听本机地址 `127.0.0.1:30010`。
- Cloudflare Tunnel 负责把 `api.alerts.example.com` 转发到 VPS。
- 不需要为本项目运行 Caddy。
- 不要向公网开放 TCP 30010。

## 2. VPS 系统要求

推荐系统：

- Rocky Linux 9 x86_64
- AlmaLinux 9 x86_64
- Debian 12 x86_64

如果希望保留 CentOS 系的 `dnf`、RPM、firewalld、SELinux 使用方式，优先选 Rocky Linux 9 或 AlmaLinux 9。

不要继续使用 CentOS Linux 8：

- CentOS Linux 8 已于 2021-12-31 结束支持，不再获得安全更新。
- CentOS Linux 8 自带 glibc 2.28，当前 GitHub Actions 产物需要 glibc 2.34。
- 在 CentOS Linux 8 上直接运行可能出现 `GLIBC_2.34 not found`。
- 不建议手动替换系统 glibc，这可能导致系统不可启动。

登录 VPS 后先确认系统、架构和 glibc：

```bash
cat /etc/os-release
uname -m
getconf GNU_LIBC_VERSION
```

要求：

```text
uname -m: x86_64
glibc: 2.34 或更高
```

如果输出 `aarch64` 或 `arm64`，当前 x86_64 artifact 不能直接使用。

## 3. 低配置 VPS 建议先配置 swap

低配置 VPS 建议配置 1 GB swap，避免编译产物解压、系统升级或运行时短暂内存峰值导致 OOM。

先检查是否已有 swap：

```bash
free -h
swapon --show
```

如果没有至少 1 GB swap，执行：

```bash
(
  set -e
  SWAPFILE=/swapfile-disaster-alert

  if ! swapon --show=NAME --noheadings | grep -Fq "$SWAPFILE"; then
    sudo dd if=/dev/zero of="$SWAPFILE" bs=1M count=1024 status=progress
    sudo chmod 600 "$SWAPFILE"
    sudo mkswap "$SWAPFILE"
    sudo swapon "$SWAPFILE"
  fi

  swapon --show=NAME --noheadings | grep -Fq "$SWAPFILE"
  awk -v path="$SWAPFILE" \
    '$1 == path && $2 == "none" && $3 == "swap" { found = 1 } END { exit !found }' \
    /etc/fstab || \
    echo "$SWAPFILE none swap sw 0 0" | sudo tee -a /etc/fstab
)
```

再次确认：

```bash
free -h
swapon --show
```

swap 只是兜底，不代表可以长期依赖 swap 承载高负载。如果服务持续大量使用 swap，应升级到至少 1 GB RAM。

## 4. 安装基础工具

Rocky Linux 9 / AlmaLinux 9：

```bash
sudo dnf makecache
sudo dnf install -y \
  ca-certificates curl file firewalld iproute nano openssl procps-ng tar unzip util-linux
sudo dnf upgrade --refresh -y
```

Debian 12：

```bash
sudo apt-get update
sudo apt-get install -y \
  ca-certificates curl file iproute2 nano openssl procps tar unzip util-linux
sudo apt-get upgrade -y
```

如果系统升级了内核，建议重启后继续：

```bash
sudo reboot
```

## 5. GitHub Pages 前端设置

进入 GitHub 仓库：

1. 打开 **Settings → Pages**。
2. 将 **Source** 设置为 **GitHub Actions**。
3. 在 **Custom domain** 填入前端域名：

```text
alerts.example.com
```

4. 按 GitHub Pages 页面提示配置 DNS。
5. 等 HTTPS 证书签发完成后，启用 **Enforce HTTPS**。

GitHub Pages 自定义域名和 HTTPS 参考：

<https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site>

<https://docs.github.com/en/pages/getting-started-with-github-pages/securing-your-github-pages-site-with-https>

## 6. 配置前端使用的 API 域名

进入 GitHub 仓库：

1. 打开 **Settings → Secrets and variables → Actions → Variables**。
2. 新增仓库变量：

```text
Name: DISASTER_API_BASE
Value: https://api.alerts.example.com
```

要求：

- 必须以 `https://` 开头。
- 不要写结尾 `/`。
- 这个值是前端访问后端 API 的公网地址。

本项目的 `.github/workflows/pages.yml` 会在发布前生成：

```js
window.DISASTER_API_BASE = "https://api.alerts.example.com";
```

前端会读取这个值，把订阅、取消订阅、状态、地址搜索、测试通知等请求发到 API 域名。

## 7. 发布 GitHub Pages 前端

GitHub Pages 工作流会在 `main` 分支 push 时自动运行，也可以手动运行：

```text
Actions → GitHub Pages → Run workflow
```

发布完成后打开：

```text
https://alerts.example.com
```

如果浏览器开发者工具里看到请求发往 `https://alerts.example.com/api/...`，说明 `config.js` 没有生成正确。回到第 6 节检查 `DISASTER_API_BASE`，然后重新运行 GitHub Pages 工作流。

## 8. 下载 GitHub Actions 编译产物

进入 GitHub 仓库：

1. 打开 **Actions**。
2. 选择 `main` 分支最新的绿色 **CI** 运行。
3. 在页面底部 **Artifacts** 下载：

```text
disaster-alert-linux-x86_64-<commit SHA>
```

解压 GitHub 下载的 ZIP 后，应看到：

```text
disaster-alert-linux-x86_64.tar.gz
disaster-alert-linux-x86_64.tar.gz.sha256
```

把两个文件上传到 VPS：

```bash
scp disaster-alert-linux-x86_64.tar.gz disaster-alert-linux-x86_64.tar.gz.sha256 你的用户名@VPS_IP:/tmp/
```

也可以用 SFTP 工具上传到 `/tmp`。

Actions Artifact 当前保留 14 天。过期后可在 **Actions → CI → Run workflow** 手动重新运行生成。

## 9. 校验并安装二进制

在 VPS 上执行：

```bash
cd /tmp
sha256sum -c disaster-alert-linux-x86_64.tar.gz.sha256
tar -xzf disaster-alert-linux-x86_64.tar.gz
sudo install -m 0755 disaster-alert /usr/local/bin/disaster-alert
```

第一条命令必须输出：

```text
disaster-alert-linux-x86_64.tar.gz: OK
```

确认安装：

```bash
ls -lh /usr/local/bin/disaster-alert
file /usr/local/bin/disaster-alert
```

不要直接运行二进制；先完成环境变量、用户、目录和 systemd 配置。

## 10. 创建专用用户和目录

创建系统用户：

```bash
sudo useradd \
  --system \
  --home-dir /var/lib/disaster-alert \
  --create-home \
  --shell /usr/sbin/nologin \
  disaster-alert
```

如果提示用户已存在，可以继续。

创建数据库和配置目录：

```bash
sudo install -d -o disaster-alert -g disaster-alert -m 0750 /var/lib/disaster-alert/data
sudo install -d -o root -g disaster-alert -m 0750 /etc/disaster-alert
if ! sudo test -e /etc/disaster-alert/disaster-alert.env; then
  sudo install -o root -g disaster-alert -m 0640 /dev/null /etc/disaster-alert/disaster-alert.env
fi
```

最后三行只在环境文件不存在时创建它，不会覆盖已有配置。

## 11. 生成两个私钥

分别执行下面命令两次：

```bash
openssl rand 32 | base64 | tr '+/' '-_' | tr -d '=\n'; echo
```

你会得到两个不同的 43 字符字符串：

- 第一个写入 `DATA_ENCRYPTION_KEY`。
- 第二个写入 `ALERT_SIGNING_KEY`。

注意：

- 两个值不能相同。
- 不要把私钥发到聊天、Issue、截图或日志里。
- `DATA_ENCRYPTION_KEY` 用于加密数据库中的 Bark Key，丢失或替换后已有 Bark Key 无法恢复。
- `ALERT_SIGNING_KEY` 用于通知详情链接签名，替换后旧详情链接会失效。
- 备份数据库时必须同时安全备份环境变量文件。

## 12. 配置后端环境变量

编辑：

```bash
sudo nano /etc/disaster-alert/disaster-alert.env
```

写入下面内容，并替换域名和私钥：

```dotenv
# 只有实例运营者阅读并接受项目 README 的“使用与部署责任”后才改为 true。
INSTANCE_TERMS_ACCEPTED=false

SERVER_HOST=127.0.0.1
SERVER_PORT=30010
SHUTDOWN_TIMEOUT_SECONDS=15
ALLOWED_ORIGINS=https://alerts.example.com

DB_PATH=/var/lib/disaster-alert/data/disaster-alert.fjall
DATA_ENCRYPTION_KEY=替换为第一个私钥

ALERT_DETAIL_BASE_URL=https://api.alerts.example.com
ALERT_SIGNING_KEY=替换为第二个私钥

# 低配置 VPS 建议降低默认并发。
MAX_CONCURRENT_NOTIFICATIONS=8
HTTP_POOL_SIZE=8

RUST_LOG=info
```

关键点：

- `SERVER_HOST=127.0.0.1`：后端只允许 VPS 本机访问。
- `ALLOWED_ORIGINS=https://alerts.example.com`：只允许 GitHub Pages 前端跨域调用 API。
- `ALERT_DETAIL_BASE_URL=https://api.alerts.example.com`：Bark 通知详情链接走 API 域名。

保存后确认权限：

```bash
sudo chown root:disaster-alert /etc/disaster-alert/disaster-alert.env
sudo chmod 0640 /etc/disaster-alert/disaster-alert.env
sudo ls -l /etc/disaster-alert/disaster-alert.env
```

不要用 `cat` 把完整环境变量文件输出到终端日志或截图。

## 13. 创建 systemd 服务

创建服务文件：

```bash
sudo nano /etc/systemd/system/disaster-alert.service
```

写入：

```ini
[Unit]
Description=Disaster Alert Bark Service
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=disaster-alert
Group=disaster-alert
WorkingDirectory=/var/lib/disaster-alert
EnvironmentFile=/etc/disaster-alert/disaster-alert.env
ExecStart=/usr/local/bin/disaster-alert
Restart=on-failure
RestartSec=5s
TimeoutStopSec=20s
UMask=0077

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/disaster-alert

[Install]
WantedBy=multi-user.target
```

检查并启动：

```bash
sudo systemd-analyze verify /etc/systemd/system/disaster-alert.service
sudo systemctl daemon-reload
sudo systemctl enable --now disaster-alert
```

查看状态：

```bash
sudo systemctl status disaster-alert --no-pager
sudo journalctl -u disaster-alert -n 100 --no-pager
```

本机健康检查：

```bash
curl --fail --silent --show-error http://127.0.0.1:30010/health
echo
```

如果失败，先排查后端，不要继续配置 Tunnel。

## 14. 配置 Cloudflare Tunnel

在 VPS 上安装 `cloudflared`。安装方式以 Cloudflare 官方文档为准：

<https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/>

登录 Cloudflare：

```bash
cloudflared tunnel login
```

创建 Tunnel：

```bash
cloudflared tunnel create disaster-alert
```

创建配置目录：

```bash
sudo install -d -m 0755 /etc/cloudflared
```

把 `cloudflared tunnel create` 输出的 `<TUNNEL_UUID>.json` 凭据文件放到 `/etc/cloudflared/`。

创建配置文件：

```bash
sudo nano /etc/cloudflared/config.yml
```

写入：

```yaml
tunnel: <TUNNEL_UUID>
credentials-file: /etc/cloudflared/<TUNNEL_UUID>.json

ingress:
  - hostname: api.alerts.example.com
    service: http://127.0.0.1:30010
  - service: http_status:404
```

创建 DNS 路由：

```bash
cloudflared tunnel route dns disaster-alert api.alerts.example.com
```

安装并启动 systemd 服务：

```bash
sudo cloudflared service install
sudo systemctl enable --now cloudflared
sudo systemctl status cloudflared --no-pager
```

验证公网 API：

```bash
curl --fail --silent --show-error https://api.alerts.example.com/health
echo
```

Cloudflare Tunnel 基本配置参考：

<https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/>

## 15. 防火墙和端口

使用 Cloudflare Tunnel 时，本项目不需要 VPS 入站 80/443。

云防火墙或安全组建议只开放：

- SSH 端口，例如 TCP 22 或你自定义的 SSH 端口。
- 其他已经存在且明确需要公网访问的服务端口。

不要开放：

- TCP 30010。
- 为本项目额外开放 TCP 80/443。

如果使用 firewalld，只保留 SSH：

```bash
sudo systemctl enable --now firewalld
sudo firewall-cmd --permanent --add-service=ssh
sudo firewall-cmd --reload
sudo firewall-cmd --list-all
```

如果 SSH 使用自定义端口，不要直接套用上面的命令；应先把自定义 SSH 端口加入 firewalld，避免把自己锁在服务器外。

VPS 需要允许出站连接到：

- Cloudflare Tunnel 服务。
- `https://api.day.app`。
- Wolfx 和 FAN Studio 实时数据源。
- 反向地理编码服务。

## 16. 最终验证

依次检查：

```bash
curl --fail --silent --show-error http://127.0.0.1:30010/health
echo
curl --fail --silent --show-error https://api.alerts.example.com/health
echo
```

然后浏览器打开：

```text
https://alerts.example.com
```

在页面里验证：

1. 页面能加载状态信息。
2. 地址搜索可用。
3. Bark Key 输入后订阅成功。
4. 测试发送按钮可以收到 Bark 测试通知。
5. 刷新页面后 Bark Key 输入框仍保留本机浏览器保存的值。
6. Bark 通知详情链接能打开 `https://api.alerts.example.com/incidents/...`。

重启验证：

```bash
sudo systemctl restart disaster-alert
sleep 3
curl --fail --silent --show-error http://127.0.0.1:30010/health
echo
curl --fail --silent --show-error https://api.alerts.example.com/health
echo
```

## 17. 日常查看命令

```bash
# 应用状态
sudo systemctl status disaster-alert --no-pager

# 最近 200 行应用日志
sudo journalctl -u disaster-alert -n 200 --no-pager

# 持续查看应用日志
sudo journalctl -u disaster-alert -f

# Tunnel 状态
sudo systemctl status cloudflared --no-pager

# 最近 200 行 Tunnel 日志
sudo journalctl -u cloudflared -n 200 --no-pager

# 资源使用
free -h
swapon --show
df -h
ps -o pid,user,%cpu,%mem,rss,cmd -C disaster-alert -C cloudflared
```

低配置 VPS 建议限制 systemd journal 占用：

```bash
sudo mkdir -p /etc/systemd/journald.conf.d
sudo nano /etc/systemd/journald.conf.d/size-limit.conf
```

写入：

```ini
[Journal]
SystemMaxUse=200M
RuntimeMaxUse=50M
```

应用：

```bash
sudo systemctl restart systemd-journald
sudo journalctl --disk-usage
```

## 18. 更新版本

从 `main` 分支最新绿色 CI 运行下载新的 artifact，上传到 `/tmp` 后执行：

```bash
cd /tmp
sha256sum -c disaster-alert-linux-x86_64.tar.gz.sha256
tar -xzf disaster-alert-linux-x86_64.tar.gz

sudo systemctl stop disaster-alert
sudo cp -a /usr/local/bin/disaster-alert /usr/local/bin/disaster-alert.previous
sudo install -m 0755 disaster-alert /usr/local/bin/disaster-alert
sudo systemctl start disaster-alert

sudo systemctl status disaster-alert --no-pager
curl --fail --silent --show-error http://127.0.0.1:30010/health
echo
```

不要覆盖：

- `/etc/disaster-alert/disaster-alert.env`
- `/var/lib/disaster-alert/data`

如果新版本启动失败，回滚：

```bash
sudo systemctl stop disaster-alert
sudo install -m 0755 /usr/local/bin/disaster-alert.previous /usr/local/bin/disaster-alert
sudo systemctl start disaster-alert
sudo journalctl -u disaster-alert -n 100 --no-pager
```

## 19. 备份和恢复

数据库使用 `DATA_ENCRYPTION_KEY` 加密敏感记录，因此数据库和环境变量文件必须成套备份。

备份：

```bash
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
sudo systemctl stop disaster-alert
sudo tar -C / \
  -czf "/root/disaster-alert-backup-${stamp}.tar.gz" \
  etc/disaster-alert/disaster-alert.env \
  var/lib/disaster-alert/data
sudo chmod 0600 "/root/disaster-alert-backup-${stamp}.tar.gz"
sudo systemctl start disaster-alert
sudo ls -lh "/root/disaster-alert-backup-${stamp}.tar.gz"
```

把备份复制到另一台设备或加密备份存储。只保存在同一块 VPS 磁盘上不算有效备份。

恢复前先停止应用，并保留当前数据副本：

```bash
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
sudo systemctl stop disaster-alert
sudo cp -a /etc/disaster-alert/disaster-alert.env \
  "/root/disaster-alert.env.before-restore-${stamp}"
sudo chmod 0600 "/root/disaster-alert.env.before-restore-${stamp}"
sudo mv /var/lib/disaster-alert/data "/var/lib/disaster-alert/data.before-restore-${stamp}"
sudo tar -C / -xzf /root/disaster-alert-backup-替换时间.tar.gz
sudo chown -R disaster-alert:disaster-alert /var/lib/disaster-alert/data
sudo chown root:disaster-alert /etc/disaster-alert/disaster-alert.env
sudo chmod 0640 /etc/disaster-alert/disaster-alert.env
sudo systemctl start disaster-alert
```

## 20. 常见问题

### 应用启动失败

查看：

```bash
sudo systemctl status disaster-alert --no-pager
sudo journalctl -u disaster-alert -n 200 --no-pager
```

重点检查：

- `DATA_ENCRYPTION_KEY` 和 `ALERT_SIGNING_KEY` 是否都是 32 字节随机值编码成的 43 字符无填充 Base64URL。
- 两个私钥是否不同。
- `DB_PATH` 的父目录是否属于 `disaster-alert` 用户。
- 30010 是否被其他进程占用。

```bash
sudo ss -ltnp | grep 30010
```

### 浏览器报 CORS 错误

检查：

```dotenv
ALLOWED_ORIGINS=https://alerts.example.com
```

修改后重启：

```bash
sudo systemctl restart disaster-alert
```

### 前端请求发到了 alerts.example.com/api

说明 GitHub Pages 没有生成正确的 `config.js`。检查 GitHub Actions 仓库变量：

```text
DISASTER_API_BASE=https://api.alerts.example.com
```

然后重新运行 GitHub Pages 工作流。

### api.alerts.example.com 返回 502 或无法连接

先在 VPS 本机检查后端：

```bash
curl --fail --silent --show-error http://127.0.0.1:30010/health
sudo systemctl status disaster-alert --no-pager
```

再检查 Tunnel：

```bash
sudo systemctl status cloudflared --no-pager
sudo journalctl -u cloudflared -n 100 --no-pager
```

### Bark 通知详情打开了错误域名

检查：

```dotenv
ALERT_DETAIL_BASE_URL=https://api.alerts.example.com
```

修改后只影响之后新发送的通知链接，旧通知链接不会自动改写。

### 出现 OOM 或服务被杀

检查：

```bash
free -h
swapon --show
sudo journalctl -k --no-pager | grep -Ei 'out of memory|oom|killed process'
```

确认环境变量中已经设置：

```dotenv
MAX_CONCURRENT_NOTIFICATIONS=8
HTTP_POOL_SIZE=8
```

如果仍持续 OOM，应升级到至少 1 GB RAM，而不是不断增加 swap。

## 21. 安全检查清单

- [ ] 前端域名是 `https://alerts.example.com`。
- [ ] API 域名是 `https://api.alerts.example.com`。
- [ ] GitHub Actions 变量 `DISASTER_API_BASE=https://api.alerts.example.com` 已配置。
- [ ] 应用只监听 `127.0.0.1:30010`。
- [ ] 云防火墙没有向公网开放 `30010`。
- [ ] 本项目没有占用 VPS 的 80/443。
- [ ] `ALLOWED_ORIGINS` 只包含可信前端 Origin。
- [ ] `ALERT_DETAIL_BASE_URL` 指向 API 域名。
- [ ] `/etc/disaster-alert/disaster-alert.env` 权限是 `0640 root:disaster-alert`。
- [ ] 两个私钥不同，并且有安全的离线备份。
- [ ] 数据库目录只由 `disaster-alert` 用户写入。
- [ ] 已完成一次数据库和环境变量文件的成套备份。
- [ ] 已验证 VPS 重启后 `disaster-alert` 和 `cloudflared` 会自动恢复。
- [ ] 只有实例运营者阅读并接受责任声明后才设置 `INSTANCE_TERMS_ACCEPTED=true`。
