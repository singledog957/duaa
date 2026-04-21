#!/usr/bin/env bash
set -euo pipefail

# easy_sign.sh - Two-phase auto-checkin
#
# Phase 1 (early morning, e.g., 07:00 daily):
#   ./easy_sign.sh --query
#   - Queries today's schedules for all students
#   - Caches schedules locally
#   - Dynamically updates crontab with per-course checkin tasks
#   - Avoids duplicate cron entries
#
# Phase 2 (per-course, ~10 min before class):
#   ./easy_sign.sh --checkin <student_id> <schedule_id>
#   - Executes checkin immediately with pre-queried data
#   - Exits quickly after completion

CONFIG_PATH="config.json"
STATE_DIR="./state"
CRON_MARKER="duaa-checkin"  # Used to mark and track scheduled tasks
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT_PATH="$SCRIPT_DIR/easy_sign.sh"

LOGIN_URL="https://iclass.buaa.edu.cn:8347/app/user/login.action"
SCHEDULE_URL="https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action"
CHECKIN_URL="http://iclass.buaa.edu.cn:8081/app/course/stu_scan_sign.action"
TIMESTAMP_URL="http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action"

UA="Mozilla/5.0 (Linux; Android 13; M2012K11AC Build/TKQ1.220829.002; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 wxwork/4.1.22 MicroMessenger/7.0.1 NetType/WIFI Language/zh ColorScheme/Light"

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
Usage: ./easy_sign.sh --query [--config PATH] [--state-dir PATH]
       ./easy_sign.sh --checkin <student_id> <schedule_id> [second_offset]

Phases:
  --query                          Query schedules and update crontab (run early morning)
  --checkin STUDENT_ID SCHEDULE_ID [second_offset]
                                   Execute checkin for specific course

Options for --query:
  --config PATH                    Path to config.json (default: ./config.json)
  --state-dir PATH                 Directory for schedule cache (default: ./state)

Examples:
  # Add to crontab:
  0 7 * * * /path/to/duaa/easy_sign.sh --query
  
  # Per-course checkins are auto-added by --query phase

EOF
}

# Parse arguments
MODE=""
CONFIG_PATH="config.json"
STATE_DIR="./state"
STUDENT_ID=""
SCHEDULE_ID=""
SECOND_OFFSET="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --query)
      MODE="query"
      shift
      ;;
    --checkin)
      MODE="checkin"
      STUDENT_ID="${2:-}"
      SCHEDULE_ID="${3:-}"
      if [[ $# -ge 4 && "${4}" =~ ^[0-9]+$ ]]; then
        SECOND_OFFSET="${4}"
        shift 4
      else
        SECOND_OFFSET="0"
        shift 3
      fi
      ;;
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

if [[ -z "$MODE" ]]; then
  echo "Error: must specify --query or --checkin" >&2
  usage
  exit 1
fi

need_cmd curl
need_cmd jq
need_cmd date

today_yyyymmdd="$(date +%Y%m%d)"
today_state_dir="$STATE_DIR/$today_yyyymmdd"

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

# Fetch exact server timestamp from iclass
get_server_timestamp() {
  local raw ts
  raw="$(curl -ks "$TIMESTAMP_URL")"
  ts="$(echo "$raw" | jq -r '.timestamp // empty' 2>/dev/null || true)"
  if [[ -z "$ts" || "$ts" == "null" ]]; then
    echo "" >&2
    return 1
  fi
  echo "$ts"
}

# Perform a single checkin request with the server timestamp
do_checkin_with_ts() {
  local user_id="$1"
  local session_id="$2"
  local sched_id="$3"
  local ts="$4"

  curl -ks -X POST "${CHECKIN_URL}" \
    -H "Sessionid: ${session_id}" \
    -H "User-Agent: $UA" \
    --get \
    --data-urlencode "id=${user_id}" \
    --data-urlencode "courseSchedId=${sched_id}" \
    --data-urlencode "timestamp=${ts}"
}

checkin() {
  local user_id="$1"
  local session_id="$2"
  local sched_id="$3"
  local ts
  ts="$(get_server_timestamp)" || {
    echo '{"ERRMSG":"Failed to get server timestamp"}' >&2
    return 1
  }

  do_checkin_with_ts "$user_id" "$session_id" "$sched_id" "$ts"
}

# Convert "2026-03-12 14:00" to cron expression "0 14 12 3 *"
# Note: The function already converts to UTC+8 (Asia/Shanghai) as needed
cron_time_from_classtime() {
  local classtime="$1"  # Format: "2026-03-12 14:00"
  local epoch
  
  epoch="$(date -d "$classtime" +%s 2>/dev/null || echo "")"
  if [[ -z "$epoch" ]]; then
    echo "" # invalid date
    return 1
  fi
  
  local hour minute day month
  hour="$(date -d "@$epoch" +%H)"
  minute="$(date -d "@$epoch" +%M)"
  day="$(date -d "@$epoch" +%d)"
  month="$(date -d "@$epoch" +%m)"
  
  # Convert to decimal to remove leading zeros
  hour=$((10#$hour))
  minute=$((10#$minute))
  day=$((10#$day))
  month=$((10#$month))
  
  printf "%d %d %d %d *" "$minute" "$hour" "$day" "$month"
}

# Subtract N minutes from a timestamp and return new cron expression
# Args: classtime (e.g., "2026-03-12 14:00"), minutes_to_subtract
subtract_minutes_and_format_cron() {
  local classtime="$1"
  local mins_to_subtract="${2:-0}"
  
  local epoch
  epoch="$(date -d "$classtime" +%s 2>/dev/null || echo "")"
  if [[ -z "$epoch" ]]; then
    echo "" # invalid date
    return 1
  fi
  
  # Subtract N minutes
  local new_epoch=$((epoch - mins_to_subtract * 60))
  
  local hour minute day month
  hour="$(date -d "@$new_epoch" +%H)"
  minute="$(date -d "@$new_epoch" +%M)"
  day="$(date -d "@$new_epoch" +%d)"
  month="$(date -d "@$new_epoch" +%m)"
  
  # Convert to decimal to remove leading zeros
  hour=$((10#$hour))
  minute=$((10#$minute))
  day=$((10#$day))
  month=$((10#$month))
  
  printf "%d %d %d %d *" "$minute" "$hour" "$day" "$month"
}

# Add a cron job for a specific course checkin
# Random trigger time: classBeginTime - random(0-600 seconds)
# Returns 0 if added, 1 if already exists
crontab_add_job() {
  local student_id="$1"
  local schedule_id="$2"
  local class_begin="$3"  # e.g., "2026-03-12 14:00"
  local course_name="$4"
  
  # Random offset in [0, 600] seconds
  local random_offset=$((RANDOM % 601))
  local offset_min=$((random_offset / 60))
  local offset_sec=$((random_offset % 60))
  
  # Calculate trigger time (classBeginTime - offset_min minutes)
  local cron_time
  cron_time="$(subtract_minutes_and_format_cron "$class_begin" "$offset_min")" || {
    log "[$student_id][$course_name] skip cron: invalid classBeginTime=$class_begin"
    return 1
  }
  
  local marker="$CRON_MARKER:${student_id}:${schedule_id}"
  # Pass second offset as 4th argument to --checkin
  local job_cmd="$SCRIPT_PATH --checkin $student_id $schedule_id $offset_sec >/dev/null 2>&1"
  local cron_entry="$cron_time $job_cmd  # $marker"
  
  # Check if already exists
  if crontab -l 2>/dev/null | grep -q "$marker"; then
    log "[$student_id][$course_name] cron already tracked"
    return 1
  fi
  
  # Add to crontab
  (crontab -l 2>/dev/null || true; echo "$cron_entry") | crontab - || {
    log "[$student_id][$course_name] failed to add cron"
    return 1
  }
  
  log "[$student_id][$course_name] added cron: $cron_time (offset: ${random_offset}s = ${offset_min}min ${offset_sec}s)"
  return 0
}

# Remove all duaa-managed cron jobs
crontab_remove_duaa_jobs() {
  crontab -l 2>/dev/null | grep -v "$CRON_MARKER" | crontab - || true
  log "Cleared all duaa-managed cron jobs"
}

# ============== PHASE 1: QUERY & REGISTER ===============

phase_query() {
  if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "Config file not found: $CONFIG_PATH" >&2
    exit 1
  fi

  if ! jq empty "$CONFIG_PATH" >/dev/null 2>&1; then
    echo "Invalid JSON config: $CONFIG_PATH" >&2
    exit 1
  fi

  mkdir -p "$today_state_dir"
  
  log "=== PHASE 1: Query & Register ==="
  log "Using config: $CONFIG_PATH"
  log "State directory: $today_state_dir"

  students_count="$(jq '.students | length' "$CONFIG_PATH")"
  if [[ "$students_count" -eq 0 ]]; then
    log "No students found in config"
    exit 0
  fi

  # First pass: collect all courses to be scheduled
  local courses_to_schedule=()
  
  while IFS= read -r stu; do
    student_id="$(echo "$stu" | jq -r '.student_id // empty')"
    student_name="$(echo "$stu" | jq -r '.name // .student_id // empty')"

    if [[ -z "$student_id" ]]; then
      continue
    fi

    # Skip student with empty course_ids
    if [[ "$(echo "$stu" | jq '.course_ids | length')" -eq 0 ]]; then
      log "[$student_id] skip: empty course_ids"
      continue
    fi

    log "[$student_id] querying schedule..."
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

    # Filter by configured course_ids and unsigned classes
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
      
      courses_to_schedule+=("$student_id|$schedule_id|$class_begin|$course_name")
    done < <(echo "$filtered" | jq -c '.[]')
  done < <(jq -c '.students[]' "$CONFIG_PATH")

  # Second pass: update crontab with new jobs
  log "Registering ${#courses_to_schedule[@]} course(s) to crontab..."
  for course_entry in "${courses_to_schedule[@]}"; do
    IFS='|' read -r student_id schedule_id class_begin course_name <<< "$course_entry"
    # "already tracked" should not fail the whole phase.
    crontab_add_job "$student_id" "$schedule_id" "$class_begin" "$course_name" || true
  done
  
  log "Phase 1 complete. Crontab updated."
}

# ============== PHASE 2: EXECUTE CHECKIN ===============

phase_checkin() {
  if [[ -z "$STUDENT_ID" || -z "$SCHEDULE_ID" ]]; then
    echo "Error: --checkin requires STUDENT_ID and SCHEDULE_ID" >&2
    exit 1
  fi

  # Wait for second offset if specified
  if (( SECOND_OFFSET > 0 )); then
    log "=== PHASE 2: Execute Checkin (with ${SECOND_OFFSET}s offset) ==="
    sleep "$SECOND_OFFSET"
  else
    log "=== PHASE 2: Execute Checkin ==="
  fi
  
  log "Student: $STUDENT_ID, ScheduleID: $SCHEDULE_ID"

  # Load cached schedule
  local cached_schedule="$today_state_dir/schedule_${STUDENT_ID}.json"
  if [[ ! -f "$cached_schedule" ]]; then
    log "No cached schedule for student $STUDENT_ID. Re-querying..."
    
    # Quick re-query
    login_resp="$(login_student "$STUDENT_ID")"
    login_status="$(echo "$login_resp" | jq -r '.STATUS // ""')"
    if [[ "$login_status" != "0" ]]; then
      log "Login failed, exiting"
      exit 1
    fi
    
    user_id="$(echo "$login_resp" | jq -r '.result.id // empty')"
    session_id="$(echo "$login_resp" | jq -r '.result.sessionId // empty')"
    if [[ -z "$user_id" || -z "$session_id" ]]; then
      log "Login response missing user/session id, exiting"
      exit 1
    fi
    
    day_resp="$(query_schedule "$user_id" "$session_id" "$today_yyyymmdd")"
  else
    # Load from cache and re-login for fresh session
    login_resp="$(login_student "$STUDENT_ID")"
    login_status="$(echo "$login_resp" | jq -r '.STATUS // ""')"
    if [[ "$login_status" != "0" ]]; then
      log "Login failed, exiting"
      exit 1
    fi
    
    user_id="$(echo "$login_resp" | jq -r '.result.id // empty')"
    session_id="$(echo "$login_resp" | jq -r '.result.sessionId // empty')"
    
    day_resp="$(cat "$cached_schedule")"
  fi

  # Verify course still exists and needs signing
  local target_status
  target_status="$(echo "$day_resp" | jq -r --arg sid "$SCHEDULE_ID" '.result // [] | map(select(.id == $sid)) | if length == 0 then "MISSING" else (.[0].signStatus // "0") end' 2>/dev/null || echo "ERROR")"
  
  local course_name
  course_name="$(echo "$day_resp" | jq -r --arg sid "$SCHEDULE_ID" '.result // [] | map(select(.id == $sid)) | if length == 0 then "unknown" else .[0].courseName // "unknown" end' 2>/dev/null || echo "unknown")"
  
  if [[ "$target_status" == "MISSING" ]]; then
    log "[$STUDENT_ID][$course_name] skip: schedule not found (may be dropped)"
    exit 0
  fi
  
  if [[ "$target_status" == "1" ]]; then
    log "[$STUDENT_ID][$course_name] skip: already signed"
    exit 0
  fi
  
  if [[ "$target_status" == "ERROR" ]]; then
    log "[$STUDENT_ID][$course_name] error parsing schedule cache, exiting"
    exit 1
  fi

  # Execute checkin
  sign_resp="$(checkin "$user_id" "$session_id" "$SCHEDULE_ID")"
  sign_status="$(echo "$sign_resp" | jq -r '.STATUS // ""')"
  sign_msg="$(echo "$sign_resp" | jq -r '.ERRMSG // ""')"

  if [[ "$sign_status" == "0" ]]; then
    log "[$STUDENT_ID][$course_name] ✓ checkin ok"
    exit 0
  else
    log "[$STUDENT_ID][$course_name] ✗ checkin failed: status=$sign_status msg=$sign_msg"
    exit 1
  fi
}

# ============== MAIN ===============

case "$MODE" in
  query)
    phase_query
    ;;
  checkin)
    mkdir -p "$today_state_dir"
    phase_checkin
    ;;
  *)
    echo "Invalid mode: $MODE" >&2
    exit 1
    ;;
esac

