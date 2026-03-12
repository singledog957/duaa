#!/usr/bin/env bash
set -euo pipefail

# easy_sign.sh
# Simple daily runner:
# 1) Query today's schedules for all students from config.json
# 2) Save schedules/plan to local state files
# 3) Trigger check-in at a random time within 10 minutes before class start

CONFIG_PATH="config.json"
STATE_DIR="./state"

LOGIN_URL="https://iclass.buaa.edu.cn:8347/app/user/login.action"
SCHEDULE_URL="https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action"
CHECKIN_URL="http://iclass.buaa.edu.cn:8081/app/course/stu_scan_sign.action"

UA="Mozilla/5.0 (Linux; Android 13; Pixel 7 Build/TQ3A.230901.001; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36"

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
Usage: ./easy_sign.sh [--config PATH] [--state-dir PATH]

Options:
  --config PATH     Path to config.json (default: ./config.json)
  --state-dir PATH  Directory for daily schedule/plan files (default: ./state)
  -h, --help        Show help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      CONFIG_PATH="${2:-}"
      shift 2
      ;;
    --state-dir)
      STATE_DIR="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

need_cmd curl
need_cmd jq
need_cmd date

if [[ ! -f "$CONFIG_PATH" ]]; then
  echo "Config file not found: $CONFIG_PATH" >&2
  exit 1
fi

if ! jq empty "$CONFIG_PATH" >/dev/null 2>&1; then
  echo "Invalid JSON config: $CONFIG_PATH" >&2
  exit 1
fi

mkdir -p "$STATE_DIR"

today_yyyymmdd="$(date +%Y%m%d)"
today_state_dir="$STATE_DIR/$today_yyyymmdd"
mkdir -p "$today_state_dir"

login_student() {
  local student_id="$1"
  curl -ksG "$LOGIN_URL" \
    -H "User-Agent: $UA" \
    --data-urlencode "phone=$student_id" \
    --data-urlencode "password=" \
    --data-urlencode "verificationType=2" \
    --data-urlencode "verificationUrl=" \
    --data-urlencode "userLevel=1"
}

query_schedule() {
  local user_id="$1"
  local session_id="$2"
  local date_str="$3"
  curl -ks -X POST "${SCHEDULE_URL}?id=${user_id}" \
    -H "Sessionid: ${session_id}" \
    -H "User-Agent: $UA" \
    --get \
    --data-urlencode "dateStr=${date_str}"
}

checkin() {
  local user_id="$1"
  local session_id="$2"
  local sched_id="$3"
  local ts_ms
  ts_ms="$(date +%s%3N)"

  curl -ks -X POST "${CHECKIN_URL}?id=${user_id}" \
    -H "Sessionid: ${session_id}" \
    -H "User-Agent: $UA" \
    --get \
    --data-urlencode "courseSchedId=${sched_id}" \
    --data-urlencode "timestamp=${ts_ms}"
}

# Run one task in background: sleep until trigger time, then login + verify + checkin.
run_checkin_task() {
  local student_id="$1"
  local student_name="$2"
  local course_id="$3"
  local course_name="$4"
  local schedule_id="$5"
  local class_begin="$6"
  local trigger_epoch="$7"

  (
    local now
    now="$(date +%s)"
    local sleep_secs=$((trigger_epoch - now))

    if (( sleep_secs > 0 )); then
      log "[$student_id][$course_name] sleep ${sleep_secs}s until random trigger"
      sleep "$sleep_secs"
    fi

    local login_resp status user_id session_id
    login_resp="$(login_student "$student_id")"
    status="$(echo "$login_resp" | jq -r '.STATUS // ""')"
    if [[ "$status" != "0" ]]; then
      log "[$student_id][$course_name] login failed before checkin"
      exit 0
    fi

    user_id="$(echo "$login_resp" | jq -r '.result.id // empty')"
    session_id="$(echo "$login_resp" | jq -r '.result.sessionId // empty')"
    if [[ -z "$user_id" || -z "$session_id" ]]; then
      log "[$student_id][$course_name] missing user/session id"
      exit 0
    fi

    # Dynamic validation: query today's schedule again before checkin.
    local day_resp day_status target_status
    day_resp="$(query_schedule "$user_id" "$session_id" "$today_yyyymmdd")"
    day_status="$(echo "$day_resp" | jq -r '.STATUS // ""')"
    if [[ "$day_status" != "0" ]]; then
      log "[$student_id][$course_name] pre-check schedule query failed"
      exit 0
    fi

    target_status="$(echo "$day_resp" | jq -r --arg sid "$schedule_id" '.result // [] | map(select(.id == $sid)) | if length == 0 then "MISSING" else (.[0].signStatus // "0") end')"
    if [[ "$target_status" == "MISSING" ]]; then
      log "[$student_id][$course_name] skip: schedule missing"
      exit 0
    fi
    if [[ "$target_status" == "1" ]]; then
      log "[$student_id][$course_name] skip: already signed"
      exit 0
    fi

    local sign_resp sign_status sign_msg
    sign_resp="$(checkin "$user_id" "$session_id" "$schedule_id")"
    sign_status="$(echo "$sign_resp" | jq -r '.STATUS // ""')"
    sign_msg="$(echo "$sign_resp" | jq -r '.ERRMSG // ""')"

    if [[ "$sign_status" == "0" ]]; then
      log "[$student_id][$course_name] checkin ok"
    else
      log "[$student_id][$course_name] checkin failed: status=$sign_status msg=$sign_msg"
    fi
  ) &
}

log "Using config: $CONFIG_PATH"
log "State directory: $today_state_dir"

students_count="$(jq '.students | length' "$CONFIG_PATH")"
if [[ "$students_count" -eq 0 ]]; then
  log "No students found in config"
  exit 0
fi

plan_file="$today_state_dir/checkin_plan.jsonl"
: > "$plan_file"

while IFS= read -r stu; do
  student_id="$(echo "$stu" | jq -r '.student_id // empty')"
  student_name="$(echo "$stu" | jq -r '.name // .student_id // empty')"

  if [[ -z "$student_id" ]]; then
    continue
  fi

  # Skip student with empty course_ids.
  if [[ "$(echo "$stu" | jq '.course_ids | length')" -eq 0 ]]; then
    log "[$student_id] skip: empty course_ids"
    continue
  fi

  login_resp="$(login_student "$student_id")"
  login_status="$(echo "$login_resp" | jq -r '.STATUS // ""')"
  if [[ "$login_status" != "0" ]]; then
    log "[$student_id] login failed"
    continue
  fi

  user_id="$(echo "$login_resp" | jq -r '.result.id // empty')"
  session_id="$(echo "$login_resp" | jq -r '.result.sessionId // empty')"
  if [[ -z "$user_id" || -z "$session_id" ]]; then
    log "[$student_id] login response missing user/session id"
    continue
  fi

  day_resp="$(query_schedule "$user_id" "$session_id" "$today_yyyymmdd")"
  day_status="$(echo "$day_resp" | jq -r '.STATUS // ""')"
  if [[ "$day_status" != "0" ]]; then
    log "[$student_id] query schedule failed"
    continue
  fi

  echo "$day_resp" > "$today_state_dir/schedule_${student_id}.json"

  # Filter by configured course_ids and unsigned classes.
  filtered="$(jq -c --argjson ids "$(echo "$stu" | jq '.course_ids')" '
    .result // []
    | map(select((.courseId as $cid | $ids | index($cid)) != null))
    | map(select((.signStatus // "0") != "1"))
  ' <<<"$day_resp")"

  filtered_count="$(echo "$filtered" | jq 'length')"
  if [[ "$filtered_count" -eq 0 ]]; then
    log "[$student_id] no unsigned target classes today"
    continue
  fi

  while IFS= read -r item; do
    schedule_id="$(echo "$item" | jq -r '.id')"
    course_id="$(echo "$item" | jq -r '.courseId')"
    course_name="$(echo "$item" | jq -r '.courseName // "unknown"')"
    class_begin="$(echo "$item" | jq -r '.classBeginTime')"

    begin_epoch="$(date -d "$class_begin" +%s 2>/dev/null || true)"
    if [[ -z "$begin_epoch" ]]; then
      log "[$student_id][$course_name] skip: bad classBeginTime=$class_begin"
      continue
    fi

    # Random offset in [0, 600] seconds.
    offset="$((RANDOM % 601))"
    trigger_epoch="$((begin_epoch - offset))"

    # If trigger already passed, run immediately.
    now_epoch="$(date +%s)"
    if (( trigger_epoch < now_epoch )); then
      trigger_epoch="$now_epoch"
    fi

    trigger_time="$(date -d "@${trigger_epoch}" '+%F %T')"
    jq -cn \
      --arg student_id "$student_id" \
      --arg student_name "$student_name" \
      --arg course_id "$course_id" \
      --arg course_name "$course_name" \
      --arg schedule_id "$schedule_id" \
      --arg class_begin "$class_begin" \
      --arg trigger_time "$trigger_time" \
      '{student_id:$student_id,student_name:$student_name,course_id:$course_id,course_name:$course_name,schedule_id:$schedule_id,class_begin:$class_begin,trigger_time:$trigger_time}' \
      >> "$plan_file"

    run_checkin_task "$student_id" "$student_name" "$course_id" "$course_name" "$schedule_id" "$class_begin" "$trigger_epoch"
    log "[$student_id][$course_name] planned at $trigger_time"
  done < <(echo "$filtered" | jq -c '.[]')
done < <(jq -c '.students[]' "$CONFIG_PATH")

log "All tasks scheduled. Waiting for checkins to finish..."
wait
log "Done for $today_yyyymmdd"
