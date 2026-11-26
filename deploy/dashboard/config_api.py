"""Configuration read/validate/write API for duaa (8081 & 8082 modes)."""

from __future__ import annotations

import datetime as dt
import json
import os
import re
from pathlib import Path

from monitoring import LOGS_DIR, PROJECT_ROOT, resolve_safe_log_path

_CONFIG_ENV = os.environ.get("DASHBOARD_DUAA_CONFIG", "config.json")
CONFIG_PATH = Path(_CONFIG_ENV) if Path(_CONFIG_ENV).is_absolute() else PROJECT_ROOT / _CONFIG_ENV


def _target_path(path=None) -> Path:
    p = path or CONFIG_PATH
    try:
        return p.resolve() if p.is_symlink() else p
    except OSError:
        return p


def config_mtime(path=None):
    p = _target_path(path)
    try:
        return dt.datetime.fromtimestamp(p.stat().st_mtime).astimezone().isoformat(timespec="seconds")
    except OSError:
        return None


def public_config(path=None):
    p = path or CONFIG_PATH
    if not p.is_file():
        return {
            "path": p.name,
            "exists": False,
            "modified_at": None,
            "poll_interval_minutes": 5,
            "auto_window_minutes": 15,
            "log_file": "logs/duaa.log",
            "students": [],
        }
    raw = json.loads(p.read_text(encoding="utf-8"))
    students = []
    for student in raw.get("students", []):
        password = student.get("sso_password") or student.get("password") or ""
        secret_url = (student.get("secret_url") or "").strip()
        students.append(
            {
                "student_id": student.get("student_id", ""),
                "name": student.get("name", ""),
                "password": "",
                "has_password": bool(password),
                "mode": "8081" if secret_url else "8082",
                "secret_url": secret_url,
                "course_ids": student.get("course_ids", []),
                "exclude_courses": student.get("exclude_courses", []),
            }
        )
    return {
        "path": p.name,
        "exists": True,
        "modified_at": config_mtime(p),
        "poll_interval_minutes": raw.get("poll_interval_minutes", 5),
        "auto_window_minutes": raw.get("auto_window_minutes", 15),
        "log_file": raw.get("log_file") or "logs/duaa.log",
        "students": students,
    }


def _normalize_log_file(value):
    if value is None or str(value).strip() == "":
        return "logs/duaa.log"
    if not isinstance(value, str):
        raise ValueError("log_file 必须是字符串")
    val = value.strip()
    resolved = resolve_safe_log_path(val)
    if resolved is None:
        raise ValueError("出于安全限制，log_file 必须位于 logs/ 目录下（例如 logs/duaa.log）")
    rel = resolved.relative_to(LOGS_DIR).as_posix()
    if val.startswith("/app/logs/"):
        return f"/app/logs/{rel}"
    return f"logs/{rel}"


def validate_config(payload, current_path=None):
    p = current_path or CONFIG_PATH
    if not isinstance(payload, dict):
        raise ValueError("配置必须是 JSON 对象")
    poll_interval = payload.get("poll_interval_minutes")
    auto_window = payload.get("auto_window_minutes")
    if isinstance(poll_interval, bool) or not isinstance(poll_interval, int) or not (1 <= poll_interval <= 1440):
        raise ValueError("poll_interval_minutes 必须是 1-1440 的整数")
    if isinstance(auto_window, bool) or not isinstance(auto_window, int) or not (1 <= auto_window <= 1440):
        raise ValueError("auto_window_minutes 必须是 1-1440 的整数")
    log_file = _normalize_log_file(payload.get("log_file"))
    students = payload.get("students")
    if not isinstance(students, list):
        raise ValueError("students 必须是数组")

    current = json.loads(p.read_text(encoding="utf-8")) if p.is_file() else {}
    current_passwords = {
        item.get("student_id"): item.get("sso_password") or item.get("password") or ""
        for item in current.get("students", [])
        if isinstance(item, dict)
    }

    seen = set()
    cleaned_students = []
    for index, student in enumerate(students, start=1):
        if not isinstance(student, dict):
            raise ValueError(f"第 {index} 个学生必须是对象")
        student_id = str(student.get("student_id", "")).strip()
        name = str(student.get("name", "")).strip() or student_id
        password = str(student.get("password", "") or "")
        secret_url = str(student.get("secret_url", "") or "").strip()
        course_ids = student.get("course_ids", [])
        exclude_courses = student.get("exclude_courses", [])
        if not student_id:
            raise ValueError(f"第 {index} 个学生缺少学号")
        if student_id in seen:
            raise ValueError(f"学号重复：{student_id}")
        seen.add(student_id)
        if not password and student_id not in current_passwords:
            raise ValueError(f"新学生 {student_id} 必须填写 SSO 密码")
        if not isinstance(course_ids, list) or not course_ids or not all(isinstance(x, str) and x.strip() for x in course_ids):
            raise ValueError(f"{student_id} 的签到课程不能为空")
        if not isinstance(exclude_courses, list) or not all(isinstance(x, str) for x in exclude_courses):
            raise ValueError(f"{student_id} 的排除课程格式不正确")
        if secret_url and not re.match(r"^https?://", secret_url):
            raise ValueError(f"{student_id} 的 secret_url 必须是 http(s) 地址")
        entry = {
            "student_id": student_id,
            "name": name,
            "sso_password": password or current_passwords.get(student_id, ""),
            "course_ids": [x.strip() for x in course_ids],
            "exclude_courses": [x.strip() for x in exclude_courses if x.strip()],
        }
        if secret_url:
            entry["secret_url"] = secret_url
        cleaned_students.append(entry)

    return {
        "poll_interval_minutes": poll_interval,
        "auto_window_minutes": auto_window,
        "log_file": log_file,
        "students": cleaned_students,
    }


def write_config(data, path=None):
    target = _target_path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    prev_stat = target.stat() if target.exists() else PROJECT_ROOT.stat()
    temporary = target.with_suffix(target.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary, 0o600)
    try:
        os.chown(temporary, prev_stat.st_uid, prev_stat.st_gid)
    except PermissionError:
        pass
    os.replace(temporary, target)
