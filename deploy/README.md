# Docker Compose 部署与 8081 运维看板

本目录（`deploy/`）提供 `duaa` 的可选 Docker Compose 部署套件，包含：

- **`deploy/Dockerfile` & `deploy/docker-compose.yml`**：将 `duaa` 与按需开启的运维看板容器化（`duaa` 与账号容器均配置 `restart: unless-stopped` 异常自动重启；配置通过宿主机 `config.json` 热加载）。
- **`deploy/accounts/`**：8081 官方签名模式所需的无头 i北航（企业微信）账号容器套件（每名需要走 8081 的学生 1 个容器，登录态持久化在独立 Docker volume，端口仅绑定 `127.0.0.1`）。
- **`deploy/dashboard/`**：模块化 Web 运维看板，默认仅本机常驻（`127.0.0.1:9080`），支持通过 `--expose` 临时对外暴露（默认 **10 分钟后自动销毁**并自动生成一次性口令），可在网页内创建/删除 8081 账号容器、放大扫码、配置 8081/8082 学生、查看宿主机监控与实时日志。
- **`deploy/manage.sh`**：统一管理脚本。

> **运行环境要求**：本部署套件仅支持 **Linux `x86_64`（amd64）** 宿主机，需安装 Docker 与 Docker Compose v2。部署前请确认宿主机系统时间准确（`date`）。

---

## 两种签到模式说明

在看板或 `config.json` 中，每位学生可以按需选择：

| 模式 | 原理 | 是否需要 8081 账号容器 |
|---|---|---|
| **8081 官方签名模式（推荐）** | 关联本机账号容器获取 60 秒签票（`taskid` / `signSecret`），走官方 8081 签名接口，失败自动回退 8082 | 需要（每名学生 1 个独立容器） |
| **8082 直连模式** | 仅凭学号与统一认证密码通过 SSO 直接签到 | 不需要 |

---

## 快速开始

以下命令均在**仓库根目录**执行。

### 第 1 步：准备配置文件

```bash
cp config.example.json config.json
```

可以直接编辑 `config.json`，也可以先留空学生列表，等第 3 步临时打开看板后在网页中配置。

### 第 2 步：构建 8081 账号基础镜像（仅 8081 模式需要）

> 如果全部学生都使用 **8082 直连模式**，可直接跳过本步。

本仓库**不包含**第三方闭源客户端安装包。请从北航官方企业微信页面下载 **Linux `x86_64`（`.deb`）安装包**（原包无需修改）：

- 官方下载地址：https://qywx.buaa.edu.cn/

将下载的 `weworklocal_*.deb` 放入 `deploy/accounts/` 目录后构建基础镜像：

```bash
cp /path/to/weworklocal_*.deb deploy/accounts/
bash deploy/manage.sh build
```

> 镜像构建默认使用 USTC apt 镜像源（`mirrors.ustc.edu.cn`），如需覆盖可执行 `docker build --build-arg APT_MIRROR="" -t duaa-account:latest deploy/accounts`。

### 第 3 步：启动后台服务并开启运维看板

```bash
# 启动后台常驻服务（duaa 与已有 8081 账号容器；不会自动开启看板）
bash deploy/manage.sh up

# 默认开启本机运维看板（仅监听 127.0.0.1:9080，不自动销毁）
bash deploy/manage.sh dashboard on

# 本机模式也可按需指定自动销毁时长（例如 30 分钟）
bash deploy/manage.sh dashboard on --ttl 30

# 临时对外暴露到 0.0.0.0:9080（默认 10 分钟后自动关闭并销毁容器，未设口令时自动生成一次性随机口令）
bash deploy/manage.sh dashboard on --expose

# 自定义临时暴露的自动销毁时长（例如 15 分钟；禁止设为 0）
bash deploy/manage.sh dashboard on --expose --ttl 15
```

默认通过本机或 SSH 转发访问：`ssh -L 9080:127.0.0.1:9080 user@server`，然后打开 `http://127.0.0.1:9080`。
使用 `--expose` 临时暴露到 `0.0.0.0` 时，若未提前设置 `DASHBOARD_TOKEN`，脚本会自动生成一次性随机口令并在终端打印；也可手动指定固定口令：

```bash
export DASHBOARD_TOKEN='替换为随机长口令'
bash deploy/manage.sh dashboard on --expose --ttl 15
```

浏览器弹出登录框时，用户名填 `admin`，密码填终端打印的一次性口令（或 `DASHBOARD_TOKEN`）。设置口令后，本机访问同样需要登录；所有页面和 API 均受保护。HTTP Basic 不加密传输内容，公网远程访问仍建议优先使用 SSH 转发或 HTTPS 反向代理。命令行查看倒计时时传入对应 `DASHBOARD_TOKEN` 环境变量即可。

使用完毕后，临时暴露模式会在到期后自动关闭并销毁容器，也可在页面左下角点击「**立即关闭**」或通过命令行随时关闭（不影响 `duaa` 与账号容器后台自动签到）：

```bash
bash deploy/manage.sh dashboard off
```

### 第 4 步：在看板中完成账号容器管理与签到配置

打开浏览器访问 `http://127.0.0.1:9080`（远程服务器先使用上述 SSH 转发；其他监听地址需使用口令登录），界面分为 5 个模块：

1. **总览**：查看 `duaa` 引擎、8081 账号容器数、学生模式分布、宿主机概况与最近签到日志预览。
2. **8081 账号容器**：
   - 顶部输入实例名（如 `stu1`）一键「**创建并启动**」（自动分配 `127.0.0.1:9101+`、固定 `hostname` 减少掉线、登录态持久化在独立数据卷）。
   - 点击卡片画面放大，用对应学生的手机企业微信扫码登录；若重启后停在快速登录页，启动脚本会自动切出二维码，也可在卡片上点「**弹出二维码**」或「**点 Login**」。
   - 登录后可点击「**测 Secret**」在线验证 8081 签票；也支持启停、重启或删除容器（删除时可选择是否清空登录卷）。
3. **签到配置**：
   - 点击「**+ 新增学生**」，在紧凑的卡片中填写学号、姓名（可选）、统一认证密码、签到课程（默认 `*`）与排除课程。
   - 通过「**签到方式**」下拉框一键选择 **`8082 直连（无需容器）`** 或关联已有 **`8081 · <实例名>`** 容器（支持自定义 Provider URL）。
   - 保存后学生与课程配置在下轮轮询**自动热加载**；如展开底部「全局运行参数」修改了轮询间隔或窗口，点击「**重启 duaa**」生效。
4. **监控与容器**：实时查看宿主机 CPU、内存、磁盘、网络、温度以及关联容器状态。
5. **签到日志**：按全部/成功/警告/错误筛选与关键词高亮查看 `logs/duaa.log`。

---

## 常用命令行参考

```bash
bash deploy/manage.sh build                    # 构建 8081 账号基础镜像
bash deploy/manage.sh up                       # 启动全部账号容器 + duaa（不开启看板）
bash deploy/manage.sh up duaa                  # 仅构建并启动 duaa
bash deploy/manage.sh dashboard on                   # 开启本机看板（127.0.0.1:9080，不自动销毁；可加 --ttl 分）
bash deploy/manage.sh dashboard on --expose [分]     # 临时暴露到 0.0.0.0:9080（默认 10 分钟自动销毁，自动生成一次性口令）
bash deploy/manage.sh dashboard off            # 立即关闭看板
bash deploy/manage.sh dashboard status         # 查看看板开关状态与剩余倒计时
bash deploy/manage.sh add stu1 9101            # 命令行新增 8081 账号容器
bash deploy/manage.sh login stu1               # 保存登录画面到 deploy/accounts/instances/stu1/qr.png
bash deploy/manage.sh secret stu1              # 命令行测试 stu1 获取 8081 secret
bash deploy/manage.sh status                   # 查看 duaa、看板与全部账号容器状态
bash deploy/manage.sh list                     # 列出所有 8081 账号实例与端口
bash deploy/manage.sh restart duaa             # 重启指定服务（支持 duaa / 账号名）
bash deploy/manage.sh logs duaa                # 查看容器日志
bash deploy/manage.sh rm stu1                  # 移除账号容器（保留数据卷；加 --purge 彻底清除）
bash deploy/manage.sh down                     # 停止并移除全部容器（保留登录数据卷）
```

查询当日真实签到状态：

```bash
docker exec duaa /usr/local/bin/duaa --config config.json --status
```

---

## 安全说明

- **默认仅限本机访问，对外暴露限时且强制鉴权**：运维看板挂载了 `/var/run/docker.sock` 用于管理账号容器，默认仅监听 `127.0.0.1`（不自动销毁；如在多人共用服务器上运行，建议用完执行 `dashboard off` 或设置 `DASHBOARD_TOKEN`）。使用 `--expose` 对外暴露到 `0.0.0.0` 时默认 **10 分钟后自动销毁容器**（禁止设为永久），并强制启用 `DASHBOARD_TOKEN`（未设置时自动生成一次性随机口令）；公网访问仍建议优先通过 SSH 转发或 HTTPS 反向代理。
- **8081 Provider 仅限本机**：所有账号容器端口均只绑定 `127.0.0.1:<port>`，不对外网或局域网暴露。
- **日志读取隔离**：看板接口强制限制 `log_file` 必须位于仓库 `logs/` 目录下，防止路径穿越读取配置文件等敏感文件。
- **动态上游 DNS 与日志限额**：账号容器内透明代理 `tproxy2.py` 通过容器上游 DNS 动态解析域名（无硬编码 IP），并自带 5 MiB 日志自动截断。

## 回归验证（无需 Docker 或真实账号）

```bash
cargo test --locked
node --test tests/dashboard-config.cjs
python3 -m unittest discover -s tests -p 'test_dashboard*.py'
```
