#!/usr/bin/env bash
set -euo pipefail

STUDENT_ID="${1:-}"
CONFIG_PATH="${CONFIG_PATH:-config.json}"

SSO_LOGIN_URL="https://sso.buaa.edu.cn/login"
JUMP_URL="https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter"
LOGIN_BUAA_URL="https://iclass.buaa.edu.cn:8346/eschool/app/user/login_buaa.do"
SCHEDULE_URL="https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action"
UA="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"

usage() {
  echo "Usage: ./config.sh [student_id]"
  echo "Example: ./config.sh 22373062"
  echo
  echo "该脚本会交互式读取 SSO 密码，并自动写入 config.json。"
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing dependency: $1"
    [[ "$1" == "jq" ]] && echo "Install jq with: sudo apt install jq   (or: brew install jq)"
    exit 1
  fi
}

extract_execution() {
  sed -n 's/.*name="execution" value="\([^"]*\)".*/\1/p'
}

extract_login_name() {
  sed -n 's/.*[?&#]loginName=\([^&#]*\).*/\1/p'
}

percent_decode() {
  local data="${1//+/ }"
  printf '%b' "${data//%/\\x}"
}

auto_login_iclass() {
  local student_id="$1"
  local sso_password="$2"
  local cookie
  cookie="$(mktemp)"

  local login_page execution submit_headers location login_name login_resp status class_id real_name
  login_page="$(curl -ksS -A "$UA" -c "$cookie" -b "$cookie" "$SSO_LOGIN_URL")"
  execution="$(printf '%s' "$login_page" | extract_execution | head -n1)"
  if [[ -z "$execution" ]]; then
    rm -f "$cookie"
    echo "failed to parse SSO execution" >&2
    return 1
  fi

  submit_headers="$(mktemp)"
  curl -ksS -A "$UA" -c "$cookie" -b "$cookie" \
    -D "$submit_headers" \
    -o /tmp/duaa_config_sso_body.$$ \
    -X POST "$SSO_LOGIN_URL" \
    --data-urlencode "username=$student_id" \
    --data-urlencode "password=$sso_password" \
    --data-urlencode "submit=登录" \
    --data-urlencode "type=username_password" \
    --data-urlencode "execution=$execution" \
    --data-urlencode "_eventId=submit"

  if grep -q 'continueForm' /tmp/duaa_config_sso_body.$$; then
    execution="$(extract_execution </tmp/duaa_config_sso_body.$$ | head -n1)"
    if [[ -z "$execution" ]]; then
      rm -f "$cookie" "$submit_headers" /tmp/duaa_config_sso_body.$$
      echo "failed to parse SSO risk-page execution" >&2
      return 1
    fi
    curl -ksS -A "$UA" -c "$cookie" -b "$cookie" \
      -o /dev/null \
      -X POST "$SSO_LOGIN_URL" \
      --data-urlencode "execution=$execution" \
      --data-urlencode "_eventId=ignoreAndContinue"
  fi

  location="$(curl -ksS -A "$UA" -c "$cookie" -b "$cookie" -o /dev/null -D - "$JUMP_URL" | tr -d '\r' | awk '/^location: /{print $2}' | tail -n1)"
  login_name="$(printf '%s' "$location" | extract_login_name | head -n1)"
  login_name="$(percent_decode "$login_name")"
  if [[ -z "$login_name" ]]; then
    rm -f "$cookie" "$submit_headers" /tmp/duaa_config_sso_body.$$
    echo "failed to parse login_name from jumpMyCenter redirect" >&2
    return 1
  fi

  login_resp="$(curl -ksS -A "$UA" -G "$LOGIN_BUAA_URL" \
    --data-urlencode "phone=$login_name" \
    --data-urlencode "password=" \
    --data-urlencode "verificationType=2" \
    --data-urlencode "verificationUrl=" \
    --data-urlencode "userLevel=1")"
  status="$(printf '%s' "$login_resp" | jq -r '.STATUS // ""')"
  class_id="$(printf '%s' "$login_resp" | jq -r '.result.id // empty')"
  real_name="$(printf '%s' "$login_resp" | jq -r '.result.realName // empty')"

  rm -f "$cookie" "$submit_headers" /tmp/duaa_config_sso_body.$$

  if [[ "$status" != "0" || -z "$class_id" ]]; then
    echo "iclass login failed" >&2
    return 1
  fi
  if [[ -z "$real_name" || "$real_name" == "null" ]]; then
    real_name="$student_id"
  fi

  printf '%s\n%s\n%s\n' "$login_name" "$class_id" "$real_name"
}

query_day() {
  local class_id="$1"
  local login_name="$2"
  local date_str="$3"
  curl -ksS -A "$UA" -X POST "${SCHEDULE_URL}?id=${class_id}" \
    -H "Sessionid: ${login_name}" \
    --get \
    --data-urlencode "dateStr=${date_str}"
}

if [[ -z "$STUDENT_ID" ]]; then
  read -r -p "请输入学号: " STUDENT_ID
fi

if [[ -z "$STUDENT_ID" ]]; then
  usage
  exit 1
fi

need_cmd curl
need_cmd jq
need_cmd date

read -r -s -p "请输入该学号的 SSO 密码（将写入 config.json 的 sso_password 字段）: " SSO_PASSWORD
echo

echo "[1/5] 自动登录 SSO 并解析 iclass 身份"
mapfile -t login_info < <(auto_login_iclass "$STUDENT_ID" "$SSO_PASSWORD")
if [[ "${#login_info[@]}" -lt 3 ]]; then
  echo "自动登录失败"
  exit 1
fi
LOGIN_NAME="${login_info[0]}"
CLASS_ID="${login_info[1]}"
REAL_NAME="${login_info[2]}"

echo "[2/5] 查询未来 7 天课程"
tmp_arrays="$(mktemp)"
for i in 0 1 2 3 4 5 6; do
  date_str="$(date -d "+${i} day" +%Y%m%d)"
  day_resp="$(query_day "$CLASS_ID" "$LOGIN_NAME" "$date_str")"
  day_status="$(printf '%s' "$day_resp" | jq -r '.STATUS // ""')"
  if [[ "$day_status" != "0" ]]; then
    echo "  - Skip ${date_str}: $(printf '%s' "$day_resp" | jq -r '.ERRMSG // "status error"')"
    continue
  fi
  printf '%s\n' "$day_resp" | jq -c '.result // []' >>"$tmp_arrays"
done

courses_json="$(jq -s '[.[][] | {course_id: .courseId, course_name: .courseName, week_day: .weekDay, begin: .classBeginTime}] | unique_by(.course_id)' "$tmp_arrays")"
rm -f "$tmp_arrays"

count="$(printf '%s' "$courses_json" | jq 'length')"
if [[ "$count" -eq 0 ]]; then
  echo "No courses found in next 7 days."
  exit 1
fi

echo "[3/5] 选择课程"
printf '%s' "$courses_json" | jq -r 'to_entries[] | "[\(.key + 1)] \(.value.course_name) (course_id: \(.value.course_id)) - \(.value.week_day) \(.value.begin)"'

declare -a picks=()
while true; do
  read -r -p "Select indexes (e.g. '1 3') , or 'all' to select all courses: " input
  if [[ "$input" == "all" || "$input" == "*" ]]; then
    selected_ids="$(printf '%s' "$courses_json" | jq '[.[].course_id] | unique')"
    break
  fi

  read -r -a picks <<<"$input"
  if [[ "${#picks[@]}" -eq 0 ]]; then
    echo "Please provide at least one index."
    continue
  fi

  valid=1
  idx_json="[]"
  for token in "${picks[@]}"; do
    if [[ ! "$token" =~ ^[0-9]+$ ]] || (( token < 1 || token > count )); then
      valid=0
      break
    fi
    idx_json="$(printf '%s' "$idx_json" | jq --argjson n "$token" '. + [($n - 1)]')"
  done
  if [[ "$valid" -eq 0 ]]; then
    echo "Invalid input. Use indexes in range 1..$count or all/*"
    continue
  fi

  selected_ids="$(printf '%s' "$courses_json" | jq --argjson idx "$idx_json" '[ $idx[] as $i | .[$i].course_id ] | unique')"
  break
done

echo "[4/5] 更新 $CONFIG_PATH"
if [[ ! -f "$CONFIG_PATH" ]]; then
  echo '{"poll_interval_minutes":10,"auto_window_minutes":15,"students":[]}' >"$CONFIG_PATH"
fi

if ! jq empty "$CONFIG_PATH" >/dev/null 2>&1; then
  echo "Existing $CONFIG_PATH is not valid JSON"
  exit 1
fi

exists="$(jq --arg sid "$STUDENT_ID" '[.students[]? | select(.student_id == $sid)] | length' "$CONFIG_PATH")"
if [[ "$exists" -gt 0 ]]; then
  read -r -p "Student $STUDENT_ID already exists. Overwrite? [y/N]: " yn
  case "$yn" in
    y|Y|yes|YES) ;;
    *) echo "Aborted."; exit 0 ;;
  esac
fi

jq \
  --arg sid "$STUDENT_ID" \
  --arg name "$REAL_NAME" \
  --arg sso_password "$SSO_PASSWORD" \
  --argjson selected "$selected_ids" \
  '
  .poll_interval_minutes = (.poll_interval_minutes // 10)
  | .auto_window_minutes = (.auto_window_minutes // 15)
  | .students = (.students // [])
  | if any(.students[]?; .student_id == $sid) then
      .students = (.students | map(if .student_id == $sid then .name = $name | .sso_password = $sso_password | del(.password) | del(.login_name) | .course_ids = $selected else . end))
    else
      .students += [{student_id: $sid, name: $name, sso_password: $sso_password, course_ids: $selected}]
    end
  ' "$CONFIG_PATH" >"${CONFIG_PATH}.tmp"

mv "${CONFIG_PATH}.tmp" "$CONFIG_PATH"

echo "[5/5] Done"
echo "Updated $CONFIG_PATH for student $STUDENT_ID"
echo "已写入字段：student_id, name, sso_password, course_ids"
