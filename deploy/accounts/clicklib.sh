#!/bin/bash
# 容器内点击库：登录页检测/自动切二维码 + 工作台 → 智慧教室 → 扫一扫 → 提取 secret
set -u
export DISPLAY="${DISPLAY:-:99}"
LOG=/opt/duaa/log/tproxy.log
DBG=/opt/duaa/log/clicklib.dbg
OCR_IN="/tmp/ocr.$$.png"
OCR_OUT="/tmp/ocr2.$$.png"
CROP_IN="/tmp/crop.$$.png"
trap 'rm -f "$OCR_IN" "$OCR_OUT" "$CROP_IN"' EXIT
exec 2>>"$DBG"

# 裁剪居中登录弹窗（1920x1080 下的 333x437 对话框）并识别文字
dialog_ocr() {
  import -window root -crop 360x460+780+310 +repage "$CROP_IN" 2>/dev/null || return 1
  tesseract "$CROP_IN" stdout -l chi_sim+eng --psm 6 2>/dev/null || true
}

# 是否停在“头像 + Login + Switch account”快速登录页
is_quick_login_dialog() {
  local txt
  txt=$(dialog_ocr) || return 1
  printf '%s' "$txt" | grep -qiE 'Switch[[:space:]]*account|切换账号|切换帐号'
}

# 是否停在“二维码已过期/刷新”页面
is_expired_qr_dialog() {
  local txt
  txt=$(dialog_ocr) || return 1
  printf '%s' "$txt" | grep -qiE 'Refresh|expired|刷新|过期|失效'
}

# 点击快速登录页的蓝色 Login 按钮（触发手机确认登录）
click_login() {
  xdotool mousemove 959 659 click 1
  sleep 1
  echo "ACTION=login_clicked"
}

# 若停在快速登录页则点 Switch account 弹出二维码；若二维码过期则点中间刷新
show_qr() {
  if is_quick_login_dialog; then
    xdotool mousemove 957 715 click 1
    sleep 2
    echo "ACTION=switched_to_qr"
    return 0
  fi
  if is_expired_qr_dialog; then
    xdotool mousemove 959 515 click 1
    sleep 2
    echo "ACTION=refreshed_qr"
    return 0
  fi
  echo "ACTION=no_change"
  return 0
}

# 容器启动后台执行：等待窗口出现，若卡在快速登录页则自动切出二维码方便看板扫码
auto_qr() {
  local i
  for i in $(seq 1 10); do
    sleep 3
    if is_quick_login_dialog; then
      xdotool mousemove 957 715 click 1
      sleep 2
      echo "[auto-qr] switched quick-login dialog to QR code" >&2
      return 0
    fi
  done
  return 0
}

# OCR 定位：返回"屏幕坐标 x y"
ocr_find() {
  local grep_pat="$1" x_min="$2" x_max="$3" y_min="$4" y_max="$5" mode="$6"
  import -window root "$OCR_IN"
  python3 - "$grep_pat" "$x_min" "$x_max" "$y_min" "$y_max" "$mode" "$OCR_IN" "$OCR_OUT" <<'PYEOF'
import subprocess, csv, io, sys
pat, x0, x1, y0, y1, mode, ocr_in, ocr_out = sys.argv[1], *map(int, sys.argv[2:6]), sys.argv[6], sys.argv[7], sys.argv[8]
subprocess.run(["convert", ocr_in, "-resize", "250%",
                "-colorspace", "Gray", "-normalize", ocr_out], check=True)
out = subprocess.run(["tesseract", ocr_out, "stdout", "-l", "chi_sim+eng",
                      "--psm", "11", "tsv"], capture_output=True, text=True).stdout
cands = []
for row in csv.DictReader(io.StringIO(out), delimiter="	"):
    t = row.get("text", "") or ""
    if any(k in t for k in pat.split("|")):
        try:
            x = int(row["left"]); y = int(row["top"])
            w = int(row["width"]); h = int(row["height"])
            sx = int((x + w // 2) / 2.5); sy = int((y + h // 2) / 2.5)
            if x0 <= sx <= x1 and y0 <= sy <= y1:
                cands.append((sx, sy))
        except (ValueError, KeyError):
            pass
if cands:
    best = min(cands, key=lambda c: c[1]) if mode == "top" else cands[0]
    print(best[0], best[1])
PYEOF
}

ensure_classroom() {
  # 未登录时若停在快速登录页，顺手切出二维码并快速失败，避免空跑 OCR 占满 CPU
  if is_quick_login_dialog; then
    xdotool mousemove 957 715 click 1
    return 1
  fi
  if dialog_ocr | grep -qiE "Scan[[:space:]]+the[[:space:]]+QR|Auto[[:space:]]+log[[:space:]]+in|扫码登录|二维码"; then
    return 1
  fi
  # 已在智慧教室页面（扫一扫可见）则直接返回
  if [ -n "$(ocr_find "扫" 400 900 420 580 any)" ]; then return 0; fi
  # 1. 工作台图标（左侧栏 Worksp...）
  local XY
  XY=$(ocr_find "Worksp|工作" 0 560 250 560 top)
  if [ -n "$XY" ]; then xdotool mousemove ${XY% *} ${XY#* } click 1
  else xdotool mousemove 484 370 click 1; fi
  sleep 2
  # 2. 智慧教室应用图标（网格区第一行）
  XY=$(ocr_find "智慧|教室" 560 1400 280 560 top)
  if [ -n "$XY" ]; then xdotool mousemove ${XY% *} $(( ${XY#* } + 30 )) click 1
  else xdotool mousemove 858 400 click 1; fi
  # 3. 等页面出现「扫」（可视化判据，比流量更稳：已开页面不再发请求）
  for _ in $(seq 1 12); do
    sleep 1
    [ -n "$(ocr_find "扫" 400 900 420 580 any)" ] && return 0
  done
  return 1
}

extract_login_name() {
  grep html5GetPrivateUserInfo "$LOG" | tail -1     | grep -oP 'url=\K[A-Za-z0-9%=-]+' | tail -1     | python3 -c 'import sys,urllib.parse,base64,re
u=urllib.parse.unquote(sys.stdin.read().strip())
try:
    d=base64.b64decode(u+"=="*(-len(u)%4)).decode()
    m=re.search(r"loginName=([^&]+)",d)
    print(m.group(1) if m else "")
except Exception:
    print("")'
}

get_secret() {
  ensure_classroom || return 1
  local MARK LINE TASKID SECRET
  MARK=$(wc -l < "$LOG")
  for X in 660 668 650 672 640; do
    xdotool mousemove "$X" 500 click 1
    sleep 6
    tail -n +$((MARK+1)) "$LOG" | grep -q get_sign_secret && break
  done
  LINE=$(tail -n +$((MARK+1)) "$LOG" | grep get_sign_secret | grep taskid | tail -1)
  [ -n "$LINE" ] || return 1
  TASKID=$(echo "$LINE" | grep -oP 'taskid[^a-f0-9]*\K[a-f0-9]{32}')
  SECRET=$(echo "$LINE" | grep -oP 'signSecret[^a-f0-9]*\K[a-f0-9]{32}')
  echo "TASKID=$TASKID"
  echo "SECRET=$SECRET"
  echo "LOGIN_NAME=$(extract_login_name)"
}

case "${1:-}" in
  ensure) ensure_classroom ;;
  secret) get_secret ;;
  login-click) click_login ;;
  show-qr) show_qr ;;
  auto-qr) auto_qr ;;
  *) echo "usage: clicklib.sh {ensure|secret|login-click|show-qr|auto-qr}" >&2; exit 2 ;;
esac
