#!/bin/bash
# Docker Compose 部署与多账号统一管理脚本（位于 deploy/）。
set -u
cd "$(dirname "$0")"

ACCOUNTS_DIR="$PWD/accounts"
INSTANCES_DIR="$ACCOUNTS_DIR/instances"

usage() {
  cat <<'USAGE'
用法: bash deploy/manage.sh <命令> [参数]
  build                                   构建 8081 账号基础镜像（需先放入 deploy/accounts/weworklocal*.deb）
  add NAME [HTTP_PORT]                    新增并启动 8081 账号容器（默认仅监听 127.0.0.1:9101 起）
  login NAME                              取登录画面 → deploy/accounts/instances/NAME/qr.png
  secret NAME                             直接测试取 8081 secret
  status [NAME]                           健康检查（含 duaa、看板状态与全部账号容器）
  restart|stop NAME                       生命周期管理（支持 duaa / 账号名）
  rm NAME [--purge]                       停止并移除账号容器（加 --purge 同时删除目录与数据卷）
  up|down|logs [NAME]                     管理后台常驻服务（up 不自动开启看板）
  dashboard on [MINUTES] [--local]        临时开启运维看板（默认 20 分钟后自动关闭；0=不自动关闭）
  dashboard off                           立即关闭运维看板
  dashboard status|logs                   查看看板开关状态/倒计时或日志
  list                                    列出账号实例与端口
USAGE
  exit 2
}

check_deb() {
  if ! ls "$ACCOUNTS_DIR"/weworklocal*.deb >/dev/null 2>&1; then
    echo "错误：未在 $ACCOUNTS_DIR 下找到 weworklocal*.deb 安装包。" >&2
    echo "请先从 https://qywx.buaa.edu.cn/ 下载官方 Linux x86_64 客户端 (.deb) 放到 deploy/accounts/ 目录。" >&2
    exit 1
  fi
}

next_port() {
  local p=9101 used
  used=$(grep -hoP '\d+(?=:9000)' "$INSTANCES_DIR"/*/compose.yml 2>/dev/null || true)
  while printf '%s\n' "$used" | grep -qx "$p"; do
    p=$((p + 1))
  done
  echo "$p"
}

is_root_service() {
  [ "$1" = "duaa" ] || [ "$1" = "dashboard" ]
}

write_compose() {
  local name=$1 http=$2
  mkdir -p "$INSTANCES_DIR/$name"
  cat > "$INSTANCES_DIR/$name/compose.yml" <<YAML
services:
  wwlocal:
    image: duaa-account:latest
    build:
      context: ../..
    container_name: duaa-acc-$name
    hostname: duaa-acc-$name
    extra_hosts:
      - "iclass.buaa.edu.cn:127.0.0.1"
      - "open.weixin.qq.com:127.0.0.1"
    ports:
      - "127.0.0.1:$http:9000"
    volumes:
      - duaa-acc-$name-data:/root
    restart: unless-stopped

volumes:
  duaa-acc-$name-data:
    name: duaa-acc-$name-data
YAML
}

compose() {
  local name=$1; shift
  docker compose -f "$INSTANCES_DIR/$name/compose.yml" "$@"
}

root_compose() {
  mkdir -p ../logs
  DUAA_CONFIG="${DUAA_CONFIG:-config.json}" \
    docker compose -f docker-compose.yml "$@"
}

dashboard_status_line() {
  local state
  state=$(docker inspect --format '{{.State.Status}}' duaa-dashboard 2>/dev/null || echo "stopped")
  if [ "$state" != "running" ]; then
    echo "dashboard    OFF (已关闭)"
    return
  fi
  local info
  info=$(curl -fsS --max-time 2 "http://127.0.0.1:${DASHBOARD_PORT:-9080}/healthz" 2>/dev/null || true)
  if [ -n "$info" ]; then
    python3 -c '
import json, sys
d = json.loads(sys.argv[1])
a = d.get("auto_shutdown") or {}
if a.get("enabled"):
    rem = int(a.get("remaining_seconds") or 0)
    print(f"dashboard    ON  (剩余 {rem // 60}m{rem % 60:02d}s，将于 {a.get("expires_at")} 自动关闭)")
else:
    print("dashboard    ON  (未设置自动关闭)")
' "$info" 2>/dev/null || echo "dashboard    ON"
  else
    echo "dashboard    ON  (启动中)"
  fi
}

[ $# -ge 1 ] || usage
cmd=$1; shift

case "$cmd" in
  build)
    check_deb
    docker build -t duaa-account:latest "$ACCOUNTS_DIR"
    ;;
  add)
    [ $# -ge 1 ] || usage
    name=$1
    if ! printf '%s' "$name" | grep -qE '^[a-z0-9][a-z0-9_-]{0,23}$'; then
      echo "实例名不合法（仅允许小写字母/数字/-/_，且以字母或数字开头）: $name" >&2
      exit 1
    fi
    if is_root_service "$name"; then
      echo "保留名称不可用作账号实例名: $name" >&2
      exit 1
    fi
    if [ -f "$INSTANCES_DIR/$name/compose.yml" ]; then
      echo "已存在: $name"; exit 1
    fi
    if ! docker image inspect duaa-account:latest >/dev/null 2>&1; then
      check_deb
      docker build -t duaa-account:latest "$ACCOUNTS_DIR"
    fi
    http=$(next_port)
    [ $# -ge 2 ] && http=$2
    write_compose "$name" "$http"
    compose "$name" up -d
    echo "账号容器 $name 就绪：provider=127.0.0.1:$http"
    echo "下一步: bash deploy/manage.sh dashboard on   # 临时开启看板 :9080 扫码登录（20 分钟后自动关闭）"
    ;;
  login)
    [ $# -ge 1 ] || usage
    name=$1
    port=$(grep -oP '\d+(?=:9000)' "$INSTANCES_DIR/$name/compose.yml")
    if curl -fsS "http://127.0.0.1:$port/qr.png" -o "$INSTANCES_DIR/$name/qr.png"; then
      echo "画面已保存: $INSTANCES_DIR/$name/qr.png"
      echo "用该账号的手机企业微信扫码确认"
    else
      echo "获取画面失败，请先检查账号容器状态: bash deploy/manage.sh status $name" >&2
      exit 1
    fi
    ;;
  secret)
    [ $# -ge 1 ] || usage
    name=$1
    port=$(grep -oP '\d+(?=:9000)' "$INSTANCES_DIR/$name/compose.yml")
    curl -sS "http://127.0.0.1:$port/secret"; echo
    ;;
  status)
    if [ $# -ge 1 ]; then
      name=$1
      if [ "$name" = "dashboard" ]; then
        dashboard_status_line
      elif [ "$name" = "duaa" ]; then
        root_compose ps duaa
      else
        [ -f "$INSTANCES_DIR/$name/compose.yml" ] || { echo "未知实例: $name" >&2; exit 1; }
        port=$(grep -oP '\d+(?=:9000)' "$INSTANCES_DIR/$name/compose.yml")
        printf '%-12s 127.0.0.1:%s  ' "$name" "$port"
        curl -s --max-time 5 "http://127.0.0.1:$port/health" || echo 'UNREACHABLE'
        echo
      fi
    else
      root_compose ps
      echo
      dashboard_status_line
      for d in "$INSTANCES_DIR"/*/; do
        [ -f "$d/compose.yml" ] || continue
        name=$(basename "$d")
        port=$(grep -oP '\d+(?=:9000)' "$d/compose.yml")
        printf '%-12s 127.0.0.1:%s  ' "$name" "$port"
        curl -s --max-time 5 "http://127.0.0.1:$port/health" || echo 'UNREACHABLE'
        echo
      done
    fi
    ;;
  restart|stop)
    if [ $# -ge 1 ]; then
      name=$1
      if [ "$name" = "dashboard" ]; then
        root_compose --profile dashboard "${cmd}" dashboard
      elif [ "$name" = "duaa" ]; then
        root_compose "${cmd}" duaa
      else
        compose "$name" "${cmd}"
      fi
    else
      root_compose "${cmd}"
      for d in "$INSTANCES_DIR"/*/; do
        name=$(basename "$d")
        [ -f "$d/compose.yml" ] && compose "$name" "${cmd}"
      done
    fi
    ;;
  rm)
    [ $# -ge 1 ] || usage
    name=$1
    purge=${2:-}
    [ -f "$INSTANCES_DIR/$name/compose.yml" ] && compose "$name" down --remove-orphans
    if [ "$purge" = "--purge" ]; then
      rm -rf "$INSTANCES_DIR/$name"
      docker volume rm "duaa-acc-$name-data" 2>/dev/null || true
      echo "已彻底删除账号实例与数据卷: $name"
    fi
    ;;
  up)
    if [ $# -ge 1 ]; then
      name=$1
      if [ "$name" = "dashboard" ]; then
        exec "$0" dashboard on
      elif [ "$name" = "duaa" ]; then
        root_compose up -d --build --no-deps duaa
      else
        if ls "$ACCOUNTS_DIR"/weworklocal*.deb >/dev/null 2>&1; then
          compose "$name" up -d --build
        else
          compose "$name" up -d
        fi
      fi
    else
      for d in "$INSTANCES_DIR"/*/; do
        name=$(basename "$d")
        [ -f "$d/compose.yml" ] || continue
        if ls "$ACCOUNTS_DIR"/weworklocal*.deb >/dev/null 2>&1; then
          compose "$name" up -d --build
        else
          compose "$name" up -d
        fi
      done
      root_compose up -d --build duaa
    fi
    ;;
  down)
    if [ $# -ge 1 ]; then
      name=$1
      if [ "$name" = "dashboard" ]; then
        root_compose --profile dashboard down dashboard
      elif [ "$name" = "duaa" ]; then
        root_compose down duaa
      else
        compose "$name" down
      fi
    else
      root_compose --profile dashboard down
      for d in "$INSTANCES_DIR"/*/; do
        name=$(basename "$d")
        [ -f "$d/compose.yml" ] && compose "$name" down
      done
    fi
    ;;
  logs)
    if [ $# -ge 1 ]; then
      name=$1
      if [ "$name" = "dashboard" ]; then
        root_compose --profile dashboard logs -f dashboard
      elif [ "$name" = "duaa" ]; then
        root_compose logs -f duaa
      else
        compose "$name" logs -f
      fi
    else
      root_compose logs -f duaa
    fi
    ;;
  list)
    for d in "$INSTANCES_DIR"/*/; do
      name=$(basename "$d")
      [ -f "$d/compose.yml" ] || continue
      echo "$name  provider=127.0.0.1:$(grep -oP '\d+(?=:9000)' "$d/compose.yml")"
    done
    ;;
  dashboard)
    action=${1:-status}
    [ $# -ge 1 ] && shift
    case "$action" in
      on|up)
        ttl=20
        bind="${DASHBOARD_BIND:-0.0.0.0}"
        while [ $# -ge 1 ]; do
          case "$1" in
            --local) bind="127.0.0.1" ;;
            *)
              if printf '%s' "$1" | grep -qE '^[0-9]+$'; then
                ttl="$1"
              else
                echo "无效参数: $1（用法: bash deploy/manage.sh dashboard on [分钟数，默认20，0=不自动关闭] [--local]）" >&2
                exit 2
              fi
              ;;
          esac
          shift
        done
        DASHBOARD_BIND="$bind" DASHBOARD_TTL_MINUTES="$ttl" \
          root_compose --profile dashboard up -d --build --force-recreate --no-deps dashboard
        if [ "$ttl" -gt 0 ]; then
          echo "运维看板已开启：http://${bind}:${DASHBOARD_PORT:-9080}（将在 ${ttl} 分钟后自动关闭）"
        else
          echo "运维看板已开启：http://${bind}:${DASHBOARD_PORT:-9080}（已禁用自动关闭，使用 bash deploy/manage.sh dashboard off 手动关闭）"
        fi
        ;;
      off|stop)
        root_compose --profile dashboard stop dashboard 2>/dev/null || true
        root_compose --profile dashboard rm -f dashboard 2>/dev/null || true
        echo "运维看板已关闭"
        ;;
      status)
        dashboard_status_line
        ;;
      logs)
        root_compose --profile dashboard logs -f dashboard
        ;;
      *)
        echo "用法: bash deploy/manage.sh dashboard on [MINUTES] [--local] | off | status | logs" >&2
        exit 2
        ;;
    esac
    ;;
  *) usage ;;
esac
