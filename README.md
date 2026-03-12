# duaa

`duaa` 是[不智慧教室](https://duaa.singledog233.top)的一个分支，支持自部署托管签到。

## ⚡极速版

使用 `easy_sign.sh` + `cron`即可。


- 每天七点查询当日课表
- 在每节课开始前 10 分钟内的随机时间触发签到

### 安装

```bash
chmod +x install.sh
./install.sh --mode fast
```


### 配置

```bash
chmod +x config.sh
./config.sh <学号>
```

支持输入课程编号多选（如 `1 3`），也支持 `all` 或 `*` 全选。

### 配置 cron

```bash
chmod +x easy_sign.sh
crontab -e
```

添加：

```cron
0 7 * * * /path/to/duaa/easy_sign.sh --config /path/to/duaa/config.json >> /path/to/duaa/easy_sign.log 2>&1
```

手动运行：

```bash
./easy_sign.sh --config ./config.json
```

## 📌稳定版

稳定版使用 Rust 常驻进程，推荐配合 `systemd` 保活。

稳定版优势：

- 本地会话与课表缓存：减少重复请求，降低接口抖动影响
- 随机移动端 UA 池：降低单一请求特征
- 会话失效自动重登：遇到鉴权过期可自动恢复
- 执行前动态校验：签到前再次确认课程存在且未签到
- 失败重试与抖动：瞬时失败会自动等待后重试


### 安装依赖

```bash
chmod +x install.sh
./install.sh --mode stable
```


### 构建

```bash
cargo build --release
```

### 配置

```bash
chmod +x config.sh
./config.sh <学号>
```

支持输入课程编号多选（如 `1 3`），也支持 `all` 或 `*` 全选。

### 启动

```bash
./target/release/duaa --config ./config.json
```

不传 `--config` 时，默认读取当前目录下的 `config.json`。

### systemd 保活（推荐）

1. 创建 service 文件：`/etc/systemd/system/duaa.service`

```ini
[Unit]
Description=duaa auto checkin service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<your-user>
WorkingDirectory=/path/to/duaa
ExecStart=/path/to/duaa/target/release/duaa --config /path/to/duaa/config.json
Restart=always
RestartSec=5
Environment=RUST_LOG=duaa=info

[Install]
WantedBy=multi-user.target
```

2. 启用服务

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now duaa.service
```
