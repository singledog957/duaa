# duaa

`duaa` 是一个面向北航智慧教室的自部署签到工具，支持自动查询课表并在课前执行签到。

运行前请先确认机器时间准确，可先执行：

```bash
date
```

## 功能简介

- 支持多学号配置
- 自动通过 SSO 登录并获取智慧教室会话
- 自动查询课表并筛选目标课程
- 支持 `cron` 方案与 Rust 常驻进程方案

当前配置中只需要提供：

- `student_id`
- `name`
- `sso_password`
- `course_ids`

如需托管所有课程（包括每周新增课程），将该学生的 `auto_include_new_courses` 设为 `true`，此时 `course_ids` 可以为空。

## 配置格式

`config.json` 示例：

```json
{
  "poll_interval_minutes": 10,
  "auto_window_minutes": 15,
  "log_file": "backend_pub.log",
  "students": [
    {
      "student_id": "22373000",
      "name": "张三",
      "sso_password": "your_sso_password",
      "course_ids": ["{\"course_id\":\"91999\",\"name\":\"高等数学\"}"],
      "auto_include_new_courses": false
    }
  ]
}
```

字段说明：

- `student_id`：学号
- `name`：备注名或姓名
- `sso_password`：统一认证密码
- `course_ids`：手动选择的课程目标；支持课程 ID、课程名，以及包含 `course_id` 和 `name` 的 JSON 字符串。选中一门课时，同名的其他老师课程也会签到
- `auto_include_new_courses`：设为 `true` 时托管全部课程，Rust 版会在每周日 20:00 后规划下周课表；cron 版需执行 `--weekly`
- `poll_interval_minutes`：轮询课表间隔
- `auto_window_minutes`：自动签到时间窗口参数
- `log_file`：日志文件路径

## 快速配置

可以直接使用交互式配置脚本：

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

## 方案一：`easy_sign.sh` + `cron`

适合希望简单部署、依赖较少的场景。

### 使用方式

```bash
chmod +x easy_sign.sh
./easy_sign.sh --query
```

建议将其加入 `crontab`，每天早上查询当天课表，并在周日晚上规划下周课程：

```cron
0 7 * * * /path/to/backend_pub/easy_sign.sh --query
0 20 * * 0 /path/to/backend_pub/easy_sign.sh --weekly
```

执行流程：

1. `--query` 查询当天课表；`--weekly` 为全课程托管用户查询未来 7 天
2. 自动为待签到课程生成签到任务
3. 将任务写入 `cron`
4. 在课前重新查询课表，确认准确课次仍存在且尚未签到，然后执行签到

`install.sh --mode fast` 会配置以上两条 cron。手动设置时建议传入绝对路径的 `--config` 和 `--state-dir`。cron 所在机器的时区应设为北京时间。

如需手动触发签到，也可以使用：

```bash
./easy_sign.sh --checkin <student_id> <schedule_id>
```

## 方案二：Rust 常驻进程

适合希望长期运行、自动重试、日志更完整的场景。

### 构建

```bash
cargo build --release
```

### 启动

```bash
./target/release/duaa --config ./config.json
```

如不传 `--config`，默认读取当前目录下的 `config.json`。
Rust 版启动时规划本周剩余日期，每周日 20:00 后刷新下周课表；手动选择课程仍按轮询间隔刷新当天课表。学号会去除首尾空格并统一转为小写。

## 浏览器脚本

`duaa.js`是校内签到所需脚本。

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
