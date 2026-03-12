#!/usr/bin/env bash
set -euo pipefail

STUDENT_ID="${1:-}"
CONFIG_PATH="${CONFIG_PATH:-config.json}"

LOGIN_URL="https://iclass.buaa.edu.cn:8347/app/user/login.action"
SCHEDULE_URL="https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action"

usage() {
  echo "Usage: ./config.sh <student_id>"
  echo "Example: ./config.sh <student_id>"
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing dependency: $1"
    if [[ "$1" == "jq" ]]; then
      echo "Install jq with: sudo apt install jq   (or: brew install jq)"
    fi
    exit 1
  fi
}

if [[ -z "$STUDENT_ID" ]]; then
  usage
  exit 1
fi

need_cmd curl
need_cmd jq
need_cmd date

echo "[1/5] Login as $STUDENT_ID"
login_resp="$({
  curl -ksG "$LOGIN_URL" \
    --data-urlencode "phone=$STUDENT_ID" \
    --data-urlencode "password=" \
    --data-urlencode "verificationType=2" \
    --data-urlencode "verificationUrl=" \
    --data-urlencode "userLevel=1"
} )"

login_status="$(echo "$login_resp" | jq -r '.STATUS // ""')"
if [[ "$login_status" != "0" ]]; then
  echo "Login failed: $(echo "$login_resp" | jq -r '.ERRMSG // "unknown error"')"
  exit 1
fi

USER_ID="$(echo "$login_resp" | jq -r '.result.id // empty')"
SESSION_ID="$(echo "$login_resp" | jq -r '.result.sessionId // empty')"
REAL_NAME="$(echo "$login_resp" | jq -r '.result.realName // empty')"
if [[ -z "$REAL_NAME" || "$REAL_NAME" == "null" ]]; then
  REAL_NAME="$STUDENT_ID"
fi
if [[ -z "$USER_ID" || -z "$SESSION_ID" ]]; then
  echo "Login response missing user/session id"
  exit 1
fi

echo "[2/5] Query schedules for next 7 days"
tmp_arrays="$(mktemp)"
for i in 0 1 2 3 4 5 6; do
  date_str="$(date -d "+${i} day" +%Y%m%d)"
  day_resp="$({
    curl -ks -X POST "${SCHEDULE_URL}?id=${USER_ID}" \
      -H "Sessionid: ${SESSION_ID}" \
      --get \
      --data-urlencode "dateStr=${date_str}"
  } )"

  day_status="$(echo "$day_resp" | jq -r '.STATUS // ""')"
  if [[ "$day_status" != "0" ]]; then
    echo "  - Skip ${date_str}: $(echo "$day_resp" | jq -r '.ERRMSG // "status error"')"
    continue
  fi
  echo "$day_resp" | jq -c '.result // []' >>"$tmp_arrays"
done

courses_json="$(jq -s '[.[][] | {course_id: .courseId, course_name: .courseName, week_day: .weekDay, begin: .classBeginTime}] | unique_by(.course_id)' "$tmp_arrays")"
rm -f "$tmp_arrays"

count="$(echo "$courses_json" | jq 'length')"
if [[ "$count" -eq 0 ]]; then
  echo "No courses found in next 7 days."
  exit 1
fi

echo "[3/5] Pick courses"
echo "$courses_json" | jq -r 'to_entries[] | "[\(.key + 1)] \(.value.course_name) (course_id: \(.value.course_id)) - \(.value.week_day) \(.value.begin)"'

declare -a picks=()
while true; do
  read -r -p "Select indexes (e.g. '1 3') , or 'all' to select all courses: " input
  if [[ "$input" == "all" || "$input" == "*" ]]; then
    selected_ids="$(echo "$courses_json" | jq '[.[].course_id] | unique')"
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
    if [[ ! "$token" =~ ^[0-9]+$ ]]; then
      valid=0
      break
    fi
    if (( token < 1 || token > count )); then
      valid=0
      break
    fi
    idx_json="$(echo "$idx_json" | jq --argjson n "$token" '. + [($n - 1)]')"
  done

  if [[ "$valid" -eq 0 ]]; then
    echo "Invalid input. Use indexes in range 1..$count or all/*"
    continue
  fi

  selected_ids="$(echo "$courses_json" | jq --argjson idx "$idx_json" '[ $idx[] as $i | .[$i].course_id ] | unique')"
  break
done

echo "Selected course_ids: $(echo "$selected_ids" | jq -c '.')"

echo "[4/5] Update $CONFIG_PATH"
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
    *)
      echo "Aborted."
      exit 0
      ;;
  esac
fi

jq \
  --arg sid "$STUDENT_ID" \
  --arg name "$REAL_NAME" \
  --argjson selected "$selected_ids" \
  '
  .poll_interval_minutes = (.poll_interval_minutes // 10)
  | .auto_window_minutes = (.auto_window_minutes // 15)
  | .students = (.students // [])
  | if any(.students[]?; .student_id == $sid) then
      .students = (.students | map(if .student_id == $sid then .name = $name | .course_ids = $selected else . end))
    else
      .students += [{student_id: $sid, name: $name, course_ids: $selected}]
    end
  ' "$CONFIG_PATH" >"${CONFIG_PATH}.tmp"

mv "${CONFIG_PATH}.tmp" "$CONFIG_PATH"

echo "[5/5] Done"
echo "Updated $CONFIG_PATH for student $STUDENT_ID"
