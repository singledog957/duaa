# duaa

`duaa` 是一个面向北航智慧教室的自部署签到工具，支持自动查询课表并在课前执行签到。

运行前请先确认机器时间准确，可先执行：

```bash
date
```

## 功能简介

- 支持多学号配置
- 自动通过 SSO 登录并获取智慧教室会话
- 自动查询课表并筛选目标课程（支持 `*` 全选、按课程 ID/名称匹配与排除列表）
- 支持配置热重载与 `--status` 当日签到状态查询
- 支持 **8081 官方扫码签到接口**（推荐，搭配 [`deploy/`](./deploy/README.md) 的 Docker Compose 套件与运维看板）与 **8082 直连路径**（Rust 裸机常驻进程 / `cron` 方案；注意 8082 端口可能不稳定）

当前配置中基础字段只需要提供：

- `student_id`
- `name`
- `sso_password`
- `course_ids`

## 配置格式

`config.json` 示例（完整示例见 `config.example.json`）：

```json
{
  "poll_interval_minutes": 10,
  "auto_window_minutes": 15,
  "log_file": "logs/duaa.log",
  "students": [
    {
      "student_id": "22373000",
      "name": "张三",
      "sso_password": "your_sso_password",
      "secret_url": "http://127.0.0.1:9101/secret",
      "course_ids": ["*"],
      "exclude_courses": ["体育"]
    }
  ]
}
```

字段说明：

- `student_id`：学号
- `name`：备注名或姓名（可选，留空默认使用学号）
- `sso_password`：统一认证密码
- `course_ids`：需要自动签到的课程列表，条目可以是课程 ID 或课程名；填 `["*"]` 表示全部课程自动签到
- `exclude_courses`：排除列表（可选），条目同样支持课程 ID 或课程名，优先级高于 `course_ids`——配合 `["*"]` 即可「全签但跳过某几门」
- `auto_include_new_courses`：可选布尔值（默认 `false`），为 `true` 时自动纳入本周及下周新增课程（支持按周提前拉课表，同样受 `exclude_courses` 约束）
- `secret_url`：可选的 **8081 官方扫码签到**凭据提供器地址（例如 `http://127.0.0.1:9101/secret`）；配置后优先走 8081 扫码签名接口，失败自动回退 8082；不配置则直接走 8082（8082 端口可能不稳定）
- `poll_interval_minutes`：轮询课表间隔
- `auto_window_minutes`：自动签到时间窗口参数
- `log_file`：日志文件路径

### 热重载

Rust 常驻进程会随每轮轮询检查 `config.json`（即最迟 `poll_interval_minutes` 后生效），修改配置无需重启：

- 修改 `course_ids` / `exclude_courses` / `secret_url` / 密码：立即影响新一轮任务生成；已入队的任务在执行前也会按最新配置复核，被排除的课程直接跳过
- 新增 / 移除学生：自动同步（移除学生会清理其会话缓存）
- 配置写坏（JSON 解析失败等）：保留上一份配置继续运行，并输出 `config reload failed` 警告日志

## 方案一：Docker Compose + 8081 扫码签到与运维看板（推荐）

> **为什么推荐 8081？** 8081 是智慧教室官方的**扫码签到接口**，稳定性更高；原有的 8082 端口属于非扫码直连路径，**可能不稳定**。配置 `secret_url` 后，程序会优先走 8081 官方签名签到，仅在凭据不可用时回退 8082。

本方案适合在 **Linux `x86_64`** 服务器上使用 Docker Compose 一键部署，包含 `duaa` 容器、8081 无头 i北航签票容器以及按需开启的 Web 运维看板（默认仅本机 `127.0.0.1:9080` 常驻，加 `--expose` 可临时对外暴露并默认 10 分钟自动销毁，支持网页内增删账号容器、扫码登录与热加载配置）。完整部署步骤请见：

- 👉 **[`deploy/README.md`](./deploy/README.md)**

常用入口命令：

```bash
cp config.example.json config.json
bash deploy/manage.sh build                    # 放入官方 Linux 客户端后构建 8081 基础镜像
bash deploy/manage.sh up                       # 启动 duaa 与已有 8081 账号容器
bash deploy/manage.sh dashboard on             # 开启本机运维看板（127.0.0.1:9080，不自动销毁）
bash deploy/manage.sh dashboard on --expose    # 临时暴露到 0.0.0.0:9080（默认 10 分钟自动销毁，自动生成一次性口令）
bash deploy/manage.sh dashboard off            # 手动关闭运维看板
```

## 方案二：Rust 常驻进程

适合希望裸机轻量运行、兼容 ARM / OpenWrt 等环境的场景（不配 `secret_url` 时走 8082 端口，注意 8082 端口可能不稳定；也可配合外部 8081 `secret_url` 使用）。

### 构建

```bash
cargo build --release
```

### 启动

```bash
./target/release/duaa --config ./config.json
```

如不传 `--config`，默认读取当前目录下的 `config.json`。

### 查询当日真实签到状态

```bash
./target/release/duaa --config ./config.json --status
```

一次性命令（不影响常驻进程）：为每个学生登录 iclass 并强制刷新当日课表，输出每节课的真实签到状态和课程 ID，例如：

```
[22373000 张三] ✅ 已签 2026-09-14 08:00:00 高等数学 (courseId=91999 schedId=2479193)
[22373000 张三] ⬜ 未签 2026-09-14 10:00:00 大学英语 (courseId=91998 schedId=2479194)
```

`courseId` 就是 `course_ids` / `exclude_courses` 配置里使用的课程 ID；课程合并导致 ID 变化时，用该命令查看最新 ID。

## 快速配置（脚本方式）

如使用方案二 / 方案三，可以直接使用交互式配置脚本生成基础 `config.json`：

```bash
chmod +x config.sh
./config.sh <学号>
```

也可以直接运行：

```bash
./config.sh
```

然后按提示输入学号和 SSO 密码。

脚本会自动：

1. 读取该学号的 SSO 密码
2. 自动登录并查询未来几天课程
3. 让你选择要自动签到的课程
4. 将 `student_id`、`name`、`sso_password`、`course_ids` 写入 `config.json`

## 方案三：`easy_sign.sh` + `cron`

适合希望简单部署、依赖较少的场景（走 8082 端口，可能不稳定）。

### 使用方式

```bash
chmod +x easy_sign.sh
./easy_sign.sh --query
```

建议将其加入 `crontab`，例如每天早上 7 点执行一次课表查询：

```cron
0 7 * * * /path/to/duaa/easy_sign.sh --query
```

执行流程：

1. `--query` 查询当天课表
2. 自动为待签到课程生成签到任务
3. 将任务写入 `cron`
4. 在课前自动执行签到

如需手动触发签到，也可以使用：

```bash
./easy_sign.sh --checkin <student_id> <schedule_id>
```

## 旧配置迁移

如果你之前使用过旧版本配置：

- 旧字段 `password` 现在建议改为 `sso_password`
- 旧字段 `login_name` 不再需要维护

当前版本仍兼容读取旧的 `password` 字段，但后续建议统一迁移到 `sso_password`。

## 致谢

本项目的认证与请求思路参考自
- [fontlos/buaa-api](https://github.com/fontlos/buaa-api)
- [BUAASubnet/UBAA](https://github.com/BUAASubnet/UBAA)

感谢你们的贡献❤。
