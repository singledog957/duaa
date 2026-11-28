#!/usr/bin/env bash
set -euo pipefail

script="$(cd "$(dirname "$0")/.." && pwd)/easy_sign.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
mkdir -p bin "state/$(date +%Y%m%d)"
printf 'fixture\nfixture\n' > "state/$(date +%Y%m%d)/session_test.txt"
echo '{"students":[{"student_id":"test","password":"fixture","auto_include_new_courses":true}]}' > config.json
cat > bin/curl <<'STUB'
#!/usr/bin/env bash
case "$*" in
  *get_stu_course_sched*) echo '{"STATUS":"0","result":[{"id":"lesson","courseId":"course","courseName":"Test","signStatus":"0"}]}' ;;
  *get_timestamp*) echo '{"timestamp":"1"}' ;;
  *stu_scan_sign*) printf '%s\n' "$REPLY_JSON" ;;
  *) exit 1 ;;
esac
STUB
printf '#!/usr/bin/env bash\ncat >/dev/null\n' > bin/crontab
chmod +x bin/*
export PATH="$tmp/bin:$PATH"

check_result() {
  local response="$1" expected_exit="$2" expected_log="$3" status=0 output
  output="$(REPLY_JSON="$response" bash "$script" --checkin test lesson </dev/null)" || status=$?
  if [[ "$status" != "$expected_exit" || "$output" != *"$expected_log"* ]]; then
    printf 'Unexpected result for %s: exit %s, log %s\n' "$response" "$status" "$output" >&2
    exit 1
  fi
}

check_result '{"STATUS":"0","result":{"stuSignStatus":"1"}}' 0 'checkin ok'
check_result '{"STATUS":0,"result":{"stuSignStatus":1}}' 0 'checkin ok'
check_result '{"STATUS":"0","result":{"stuSignStatus":"0"}}' 1 'checkin failed'
check_result '{"STATUS":"0","result":{}}' 1 'checkin failed'
check_result '{"STATUS":"1","result":{"stuSignStatus":"1"}}' 1 'checkin failed'
