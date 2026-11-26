#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="config.json"
STATE_DIR="./state"
CRON_MARKER="duaa-checkin"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT_PATH="$SCRIPT_DIR/easy_sign.sh"

SSO_LOGIN_URL="https://sso.buaa.edu.cn/login"
JUMP_URL="https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter"
LOGIN_BUAA_URL="https://iclass.buaa.edu.cn:8346/eschool/app/user/login_buaa.do"
SCHEDULE_URL="https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action"
CHECKIN_URL="http://iclass.buaa.edu.cn:8081/eschool/app/course/stu_scan_sign.action"
TIMESTAMP_URL="http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action"
UA="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"

log() {
  printf "[%s] %s\n" "$(date '+%F %T')" "$*"
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing dependency: $1" >&2
    exit 1
  }
}

usage() {
  cat <<'EOF'
Usage: ./easy_sign.sh --query|--weekly [--config PATH] [--state-dir PATH]
       ./easy_sign.sh --checkin <student_id> <schedule_id> [second_offset]
EOF
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
  local cookie body_file
  cookie="$(mktemp)"
  body_file="$(mktemp)"

  local login_page execution location login_name login_resp status class_id real_name
  login_page="$(curl -ksS -A "$UA" -c "$cookie" -b "$cookie" "$SSO_LOGIN_URL")"
  execution="$(printf '%s' "$login_page" | extract_execution | head -n1)"
  if [[ -z "$execution" ]]; then
    rm -f "$cookie" "$body_file"
    echo "failed to parse SSO execution" >&2
    return 1
  fi

  curl -ksS -A "$UA" -c "$cookie" -b "$cookie" \
    -o "$body_file" \
    -X POST "$SSO_LOGIN_URL" \
    --data-urlencode "username=$student_id" \
    --data-urlencode "password=$sso_password" \
    --data-urlencode "submit=登录" \
    --data-urlencode "type=username_password" \
    --data-urlencode "execution=$execution" \
    --data-urlencode "_eventId=submit"

  if grep -q 'continueForm' "$body_file"; then
    execution="$(extract_execution <"$body_file" | head -n1)"
    if [[ -z "$execution" ]]; then
      rm -f "$cookie" "$body_file"
      echo "failed to parse SSO risk-page execution" >&2
      return 1
    fi
    curl -ksS -A "$UA" -c "$cookie" -b "$cookie" \
      -o /dev/null \
      -X POST "$SSO_LOGIN_URL" \
      --data-urlencode "execution=$execution" \
      --data-urlencode "_eventId=ignoreAndContinue"
  fi

  location="$(curl -kLsS --max-redirs 10 -A "$UA" -c "$cookie" -b "$cookie" -o /dev/null -D - "$JUMP_URL" | tr -d '\r' | awk 'tolower($1) == "location:" {print $2}')"
  login_name="$(printf '%s' "$location" | extract_login_name | head -n1)"
  login_name="$(percent_decode "$login_name")"
  if [[ -z "$login_name" ]]; then
    rm -f "$cookie" "$body_file"
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

  rm -f "$cookie" "$body_file"

  if [[ "$status" != "0" || -z "$class_id" ]]; then
    echo "iclass login failed" >&2
    return 1
  fi
  if [[ -z "$real_name" || "$real_name" == "null" ]]; then
    real_name="$student_id"
  fi

  printf '%s\n%s\n%s\n' "$login_name" "$class_id" "$real_name"
}

query_schedule() {
  local class_id="$1"
  local login_name="$2"
  local date_str="$3"
  curl -ksS -A "$UA" -X POST "${SCHEDULE_URL}?id=${class_id}" \
    -H "Sessionid: ${login_name}" \
    --get \
    --data-urlencode "dateStr=${date_str}"
}

get_server_timestamp() {
  local class_id="$1"
  local login_name="$2"
  local raw ts
  raw="$(curl -ksS -A "$UA" -X POST "${TIMESTAMP_URL}?id=${class_id}" -H "Sessionid: ${login_name}")"
  ts="$(printf '%s' "$raw" | jq -r '.timestamp // empty' 2>/dev/null || true)"
  [[ -n "$ts" && "$ts" != "null" ]] || return 1
  echo "$ts"
}

do_checkin_with_ts() {
  local class_id="$1"
  local login_name="$2"
  local sched_id="$3"
  local ts="$4"
  curl -ksS -A "$UA" -X POST "${CHECKIN_URL}?id=${class_id}" \
    -H "Sessionid: ${login_name}" \
    --get \
    --data-urlencode "courseSchedId=${sched_id}" \
    --data-urlencode "timestamp=${ts}"
}

checkin() {
  local class_id="$1"
  local login_name="$2"
  local sched_id="$3"
  local ts
  ts="$(get_server_timestamp "$class_id" "$login_name")" || {
    echo '{"ERRMSG":"Failed to get server timestamp"}' >&2
    return 1
  }
  do_checkin_with_ts "$class_id" "$login_name" "$sched_id" "$ts"
}

filter_target_schedules() {
  local day_resp="$1" stu="$2"
  jq -c --argjson ids "$(printf '%s' "$stu" | jq '.course_ids // []')" --argjson automatic "$(printf '%s' "$stu" | jq '.auto_include_new_courses == true')" '
    def target: if type == "string" then (try fromjson catch .) else . end;
    (.result // []) as $schedules
    | ($ids | map(target)) as $targets
    | ($targets | map(if type == "object" then .course_id // empty else . end)) as $raw_ids
    | ($targets | map(if type == "object" then .name // empty else . end)
        + ($schedules | map(select((.courseId | tostring) as $cid | $raw_ids | index($cid) != null) | .courseName))) as $names
    | $schedules | map(select($automatic or
        ((.courseId | tostring) as $cid | $raw_ids | index($cid) != null) or
        ((.id | tostring) as $sid | $raw_ids | index($sid) != null) or
        ((.courseName as $name | $names | index($name)) != null)))
    | map(select((.signStatus | tostring) != "1"))
  ' <<<"$day_resp"
}

crontab_add_job() {
  local student_id="$1"
  local schedule_id="$2"
  local class_begin="$3"
  local course_name="$4"
  local class_epoch now_epoch max_offset random_offset
  class_epoch="$(date -d "$class_begin" +%s 2>/dev/null)" || return 1
  now_epoch="$(date +%s)"
  (( class_epoch > now_epoch )) || return 0
  if (( class_epoch - now_epoch <= 60 )); then
    log "[$student_id][$course_name] class starts within one minute; checking now"
    "$SCRIPT_PATH" --checkin "$student_id" "$schedule_id" 0 --config "$CONFIG_PATH" --state-dir "$STATE_DIR"
    return
  fi
  max_offset=$((class_epoch - now_epoch - 60))
  (( max_offset > 600 )) && max_offset=600
  if (( max_offset > 60 )); then
    random_offset=$((60 + RANDOM % (max_offset - 59)))
  else
    random_offset=0
  fi
  local target_epoch=$((class_epoch - random_offset))
  local cron_time offset_sec
  cron_time="$(date -d "@$target_epoch" '+%-M %-H %-d %-m *')"
  offset_sec="$((10#$(date -d "@$target_epoch" +%S)))"
  local marker="$CRON_MARKER:${student_id}:${schedule_id}"
  local job_cmd="$SCRIPT_PATH --checkin $student_id $schedule_id $offset_sec --config $CONFIG_PATH --state-dir $STATE_DIR >/dev/null 2>&1"
  local cron_entry="$cron_time $job_cmd  # $marker"
  local existing_cron
  existing_cron="$(crontab -l 2>/dev/null || true)"
  if printf '%s\n' "$existing_cron" | grep -Fxq "$cron_entry"; then
    log "[$student_id][$course_name] cron already tracked"
    return 0
  fi
  (printf '%s\n' "$existing_cron" | awk -v marker="$marker" '$NF != marker'; echo "$cron_entry") | crontab - || {
    log "[$student_id][$course_name] failed to add cron"
    return 1
  }
  log "[$student_id][$course_name] added cron: $cron_time (offset: ${random_offset}s)"
}

crontab_remove_job() {
  command -v crontab >/dev/null 2>&1 || return 0
  local marker="$CRON_MARKER:$1:$2"
  local existing_cron
  existing_cron="$(crontab -l 2>/dev/null || true)"
  printf '%s\n' "$existing_cron" | awk -v marker="$marker" '$NF != marker' | crontab -
}

MODE=""
STUDENT_ID=""
SCHEDULE_ID=""
SECOND_OFFSET="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --query) MODE="query"; shift ;;
    --weekly) MODE="weekly"; shift ;;
    --checkin)
      MODE="checkin"
      STUDENT_ID="${2:-}"
      SCHEDULE_ID="${3:-}"
      if [[ $# -ge 4 && "${4}" =~ ^[0-9]+$ ]]; then
        SECOND_OFFSET="${4}"
        shift 4
      else
        shift 3
      fi
      ;;
    --config) CONFIG_PATH="${2:-}"; shift 2 ;;
    --state-dir) STATE_DIR="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

[[ -n "$MODE" ]] || { echo "Error: must specify --query, --weekly or --checkin" >&2; usage; exit 1; }

CONFIG_PATH="$(cd "$(dirname "$CONFIG_PATH")" && pwd)/$(basename "$CONFIG_PATH")"
mkdir -p "$STATE_DIR"
STATE_DIR="$(cd "$STATE_DIR" && pwd)"

need_cmd curl
need_cmd jq
need_cmd date

today_yyyymmdd="$(date +%Y%m%d)"
today_state_dir="$STATE_DIR/$today_yyyymmdd"

phase_query() {
  [[ -f "$CONFIG_PATH" ]] || { echo "Config file not found: $CONFIG_PATH" >&2; exit 1; }
  jq empty "$CONFIG_PATH" >/dev/null 2>&1 || { echo "Invalid JSON config: $CONFIG_PATH" >&2; exit 1; }
  mkdir -p "$today_state_dir"

  log "=== PHASE 1: Query & Register ==="
  local courses_to_schedule=()
  while IFS= read -r stu; do
    local student_id sso_password
    student_id="$(printf '%s' "$stu" | jq -r '.student_id // empty | gsub("^\\s+|\\s+$"; "") | ascii_downcase')"
    sso_password="$(printf '%s' "$stu" | jq -r '.sso_password // .password // empty')"
    [[ -n "$student_id" && -n "$sso_password" ]] || continue
    local automatic
    automatic="$(printf '%s' "$stu" | jq -r '.auto_include_new_courses == true')"
    [[ "$MODE" != "weekly" || "$automatic" == "true" ]] || continue
    if [[ "$automatic" != "true" && "$(printf '%s' "$stu" | jq '.course_ids // [] | length')" -eq 0 ]]; then
      log "[$student_id] skip: empty course_ids"
      continue
    fi

    log "[$student_id] querying schedule..."
    local login_result
    login_result="$(auto_login_iclass "$student_id" "$sso_password")" || {
      log "[$student_id] auto login failed"
      continue
    }
    mapfile -t login_info <<<"$login_result"
    local login_name="${login_info[0]}"
    local class_id="${login_info[1]}"
    local day_resp date_str day_offset last_offset
    last_offset=0
    [[ "$MODE" == "weekly" ]] && last_offset=7
    for ((day_offset=0; day_offset<=last_offset; day_offset++)); do
    date_str="$(date -d "+$day_offset day" +%Y%m%d)"
    [[ "$MODE" != "weekly" || "$day_offset" -gt 0 ]] || continue
    day_resp="$(query_schedule "$class_id" "$login_name" "$date_str")"
    if [[ "$(printf '%s' "$day_resp" | jq -r '.STATUS // ""')" != "0" ]]; then
      log "[$student_id][$date_str] query schedule failed"
      continue
    fi

    local filtered filtered_count
    filtered="$(filter_target_schedules "$day_resp" "$stu")"
    filtered_count="$(printf '%s' "$filtered" | jq 'length')"
    if [[ "$filtered_count" -eq 0 ]]; then
      log "[$student_id][$date_str] no unsigned target classes"
      continue
    fi

    while IFS= read -r item; do
      local schedule_id course_name class_begin
      schedule_id="$(printf '%s' "$item" | jq -r '.id')"
      course_name="$(printf '%s' "$item" | jq -r '.courseName // "unknown"')"
      class_begin="$(printf '%s' "$item" | jq -r '.classBeginTime')"
      courses_to_schedule+=("$student_id|$schedule_id|$class_begin|$course_name")
    done < <(printf '%s' "$filtered" | jq -c '.[]')
    done
  done < <(jq -c '.students[]' "$CONFIG_PATH")

  log "Registering ${#courses_to_schedule[@]} course(s) to crontab..."
  for course_entry in "${courses_to_schedule[@]}"; do
    IFS='|' read -r student_id schedule_id class_begin course_name <<<"$course_entry"
    crontab_add_job "$student_id" "$schedule_id" "$class_begin" "$course_name" || true
  done
}

phase_checkin() {
  [[ -n "$STUDENT_ID" && -n "$SCHEDULE_ID" ]] || { echo "Error: --checkin requires STUDENT_ID and SCHEDULE_ID" >&2; exit 1; }
  (( SECOND_OFFSET > 0 )) && sleep "$SECOND_OFFSET"
  crontab_remove_job "$STUDENT_ID" "$SCHEDULE_ID" || true
  mkdir -p "$today_state_dir"

  local sso_password
  STUDENT_ID="$(jq -nr --arg id "$STUDENT_ID" '$id | gsub("^\\s+|\\s+$"; "") | ascii_downcase')"
  sso_password="$(jq -r --arg sid "$STUDENT_ID" '.students[]? | select((.student_id | gsub("^\\s+|\\s+$"; "") | ascii_downcase) == $sid) | .sso_password // .password // empty' "$CONFIG_PATH" | head -n1)"
  [[ -n "$sso_password" ]] || { log "[$STUDENT_ID] missing sso_password in config"; exit 1; }

  local login_name class_id
  if [[ -f "$today_state_dir/session_${STUDENT_ID}.txt" ]]; then
    mapfile -t session_info <"$today_state_dir/session_${STUDENT_ID}.txt"
    login_name="${session_info[0]:-}"
    class_id="${session_info[1]:-}"
  fi
  if [[ -z "${login_name:-}" || -z "${class_id:-}" ]]; then
    local login_result
    login_result="$(auto_login_iclass "$STUDENT_ID" "$sso_password")" || {
      log "[$STUDENT_ID] auto login failed"
      exit 1
    }
    mapfile -t login_info <<<"$login_result"
    login_name="${login_info[0]}"
    class_id="${login_info[1]}"
  fi

  local day_resp
  day_resp="$(query_schedule "$class_id" "$login_name" "$today_yyyymmdd")"
  [[ "$(printf '%s' "$day_resp" | jq -r '.STATUS // ""')" == "0" ]] || { log "[$STUDENT_ID] fresh schedule query failed"; exit 1; }

  local stu selected
  stu="$(jq -c --arg sid "$STUDENT_ID" '.students[]? | select((.student_id | gsub("^\\s+|\\s+$"; "") | ascii_downcase) == $sid)' "$CONFIG_PATH" | head -n1)"
  selected="$(filter_target_schedules "$day_resp" "$stu")"
  if [[ "$(printf '%s' "$selected" | jq -r --arg sid "$SCHEDULE_ID" 'map(select((.id | tostring) == $sid)) | length')" -eq 0 ]]; then
    log "[$STUDENT_ID][$SCHEDULE_ID] skip: no longer selected or already signed"
    exit 0
  fi

  local target_status course_name
  target_status="$(printf '%s' "$day_resp" | jq -r --arg sid "$SCHEDULE_ID" '.result // [] | map(select(.id == $sid)) | if length == 0 then "MISSING" else (.[0].signStatus // "0") end' 2>/dev/null || echo "ERROR")"
  course_name="$(printf '%s' "$day_resp" | jq -r --arg sid "$SCHEDULE_ID" '.result // [] | map(select(.id == $sid)) | if length == 0 then "unknown" else .[0].courseName // "unknown" end' 2>/dev/null || echo "unknown")"
  [[ "$target_status" != "MISSING" ]] || { log "[$STUDENT_ID][$course_name] skip: schedule not found"; exit 0; }
  [[ "$target_status" != "1" ]] || { log "[$STUDENT_ID][$course_name] skip: already signed"; exit 0; }
  [[ "$target_status" != "ERROR" ]] || { log "[$STUDENT_ID][$course_name] error parsing schedule cache"; exit 1; }

  local sign_resp sign_status sign_msg
  sign_resp="$(checkin "$class_id" "$login_name" "$SCHEDULE_ID")"
  sign_status="$(printf '%s' "$sign_resp" | jq -r '.STATUS // ""')"
  sign_msg="$(printf '%s' "$sign_resp" | jq -r '.ERRMSG // ""')"
  if [[ "$sign_status" == "0" ]]; then
    log "[$STUDENT_ID][$course_name] checkin ok"
  else
    log "[$STUDENT_ID][$course_name] checkin failed: status=$sign_status msg=$sign_msg"
    exit 1
  fi
}

case "$MODE" in
  query) phase_query ;;
  checkin) phase_checkin ;;
  *) exit 1 ;;
esac
