# duaa

`duaa` 是[不智慧教室](https://duaa.singledog233.top)的一个分支，支持自部署托管签到。


<font size=4>运行前请通过`date`确保本地时间正确！</font>
## ⚡极速版

使用 `easy_sign.sh` + `cron`即可。工作流分两个阶段：

**阶段1（每天早上7点）**：查询当天课表，缓存到本地，自动为每节课**随机生成签到时间**并添加到 cron
**阶段2（各课程开始前0-10分钟内）**：cron自动触发，执行签到

### 一键安装

```bash
chmod +x install.sh
./install.sh --mode fast
```

安装脚本会自动：
- ✓ 安装依赖（curl, jq, cron）
- ✓ 配置 crontab（每天早上7点查询课表）

### 配置学生信息

```bash
chmod +x config.sh
./config.sh <学号>
```

支持输入课程编号多选（如 `1 3`），也支持 `all` 或 `*` 全选。

### 工作原理

1. **Phase 1（每天07:00）**：
   - `easy_sign.sh --query` 自动执行
   - 查询当天所有课程
   - 为每个尚未签到的课程随机生成一个签到时间（课程开始前0-10分钟）
   - 自动添加该签到任务到 crontab

2. **Phase 2（随机激活）**：
   - 当到达该课程的随机签到时间时，cron 自动触发 `easy_sign.sh --checkin`
   - 脚本验证课程仍然存在且未签到
   - 执行签到并立即退出

### 手动操作

```bash
# 手动查询课表并更新 crontab（一般不需要）
./easy_sign.sh --query --config ./config.json

# 手动为某课程签到（通常不需要）
./easy_sign.sh --checkin <student_id> <schedule_id>
```

### 查看已计划的签到任务

```bash
crontab -l | grep duaa
```

### 清除所有 duaa 计划任务

```bash
crontab -l | grep -v duaa | crontab -
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
