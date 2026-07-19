# Nova Headless Server

Nova Server 在没有桌面环境的 Linux 主机上运行完整 Nova 后端，并通过现有中转服务接受
Nova Web 的远程控制。服务器只发起出站 HTTPS/WebSocket 连接，无需开放 Nova 入站端口。

## 依赖

- `xvfb`（Nova 自动创建私有虚拟显示，窗口始终不可见）
- 至少一个已安装并登录的 Agent CLI（Codex、Claude Code、OpenCode、Devin 等）
- 可访问的 Nova Relay/Web 服务、身份 Token

Ubuntu/Debian 安装运行依赖：

```bash
sudo apt-get install xvfb libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1 librsvg2-2
```

## 启动

参数不会写回磁盘，适合由密钥管理或 systemd 注入：

```bash
Nova server \
  --relay-server https://relay.example.com \
  --token "$NOVA_TOKEN" \
  --name build-server-01 \
  --project /srv/project-a \
  --project /srv/project-b
```

也可以使用环境变量；此时命令只需 `Nova server`：

```bash
export NOVA_SERVER_RELAY_URL=https://relay.example.com
export NOVA_SERVER_TOKEN=replace-with-secret
export NOVA_SERVER_NAME=build-server-01
Nova server
```

若不传 Relay 参数，Server 会读取正式版的 `~/.nova/settings.json`。Server 模式强制开启远程
控制，但 Relay 地址和 Token 缺失时会拒绝启动。`--project` 只接受启动时已经存在的目录。

## 管理命令

所有管理命令都直接读写数据文件，不启动 Tauri、GTK 或 Xvfb。先查看完整帮助：

```bash
Nova server help
```

管理允许网页访问的项目目录：

```bash
Nova server project list
Nova server project add /srv/project-a
Nova server project remove /srv/project-a
```

删除项目只会撤销网页访问权限，不会删除目录或历史会话。项目目录是远控白名单；无头模式
不会向网页同步白名单之外的历史会话。

持久化配置：

```bash
Nova server config set relay-server https://relay.example.com
Nova server config set token replace-with-secret
Nova server config set name build-server-01
Nova server config set codex-path /usr/local/bin/codex
Nova server config set codex-enabled true
Nova server config show
```

`config show` 默认会遮盖 Token；仅在安全终端中使用 `config show --show-token` 查看完整值。
可以通过 `config unset <键>` 清空字符串配置。使用单独数据目录时，在任意命令中加入
`--data-dir /path/to/data`，或设置 `NOVA_DATA_DIR`。

检查和安装 Nova Server 更新：

```bash
Nova server update --check
Nova server update
```

更新命令按当前 Linux 架构下载 `nova-linux-{arch}-{version}.zip`，替换当前二进制并保留
`.old` 备份。安装在 `/usr/bin` 等系统目录时需要相应写权限；更新完成后需重启服务。

## systemd

创建 `/etc/nova-server.env`（权限设为 `0600`）：

```ini
NOVA_SERVER_RELAY_URL=https://relay.example.com
NOVA_SERVER_TOKEN=replace-with-secret
NOVA_SERVER_NAME=build-server-01
```

然后创建 `/etc/systemd/system/nova-server.service`：

```ini
[Unit]
Description=Nova Headless Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nova
EnvironmentFile=/etc/nova-server.env
ExecStart=/usr/bin/Nova server --project /srv/project-a
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now nova-server
journalctl -u nova-server -f
```

Token 等同远程控制凭证；应使用 HTTPS、限制环境文件权限，并只通过 `--project` 暴露必要目录。
