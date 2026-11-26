#!/usr/bin/env python3
"""duaa operations dashboard HTTP server with built-in auto-shutdown TTL."""

from __future__ import annotations

import concurrent.futures
import datetime as dt
import http.server
import json
import mimetypes
import os
import re
import socketserver
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from accounts_api import (
    account_port,
    control_account,
    create_account,
    delete_account,
    list_accounts,
    restart_duaa,
    test_account_secret,
    trigger_account_ui,
)
from config_api import CONFIG_PATH, config_mtime, public_config, validate_config, write_config
from monitoring import HOST_COLLECTOR, collect_compose, collect_logs, now_iso

STATIC = Path(__file__).resolve().parent / "static"
PORT = int(os.environ.get("DASHBOARD_PORT", "9080"))
BIND = os.environ.get("DASHBOARD_BIND", "0.0.0.0")
DEFAULT_TTL_MINUTES = max(0, int(os.environ.get("DASHBOARD_TTL_MINUTES", "20")))
ACCOUNT_NAME = re.compile(r"^[a-z0-9][a-z0-9_-]{0,23}$")
CONFIG_BODY_LIMIT = 1024 * 1024


class AutoShutdown:
    def __init__(self, server, ttl_minutes: int):
        self._server = server
        self._lock = threading.Lock()
        self._timer: threading.Timer | None = None
        self._ttl_minutes = max(0, int(ttl_minutes))
        self._deadline_mono: float | None = None
        self._expires_at: str | None = None
        if self._ttl_minutes > 0:
            self._arm_locked(self._ttl_minutes * 60)

    def _arm_locked(self, delay_seconds: float):
        if self._timer is not None:
            self._timer.cancel()
        delay_seconds = max(1.0, float(delay_seconds))
        self._deadline_mono = time.monotonic() + delay_seconds
        exp_dt = dt.datetime.now().astimezone() + dt.timedelta(seconds=delay_seconds)
        self._expires_at = exp_dt.isoformat(timespec="seconds")
        self._timer = threading.Timer(delay_seconds, self._trigger_timeout)
        self._timer.daemon = True
        self._timer.start()

    def _trigger_timeout(self):
        print(f"dashboard auto-shutdown reached ({self._ttl_minutes}m), exiting cleanly", flush=True)
        self._server.shutdown()

    def extend(self, minutes: int):
        minutes = max(1, min(1440, int(minutes)))
        with self._lock:
            self._ttl_minutes = minutes
            self._arm_locked(minutes * 60)
            return self.status_locked()

    def shutdown_now(self):
        with self._lock:
            if self._timer is not None:
                self._timer.cancel()
                self._timer = None
        threading.Thread(target=self._server.shutdown, daemon=True).start()

    def status_locked(self):
        if self._deadline_mono is None:
            return {"enabled": False, "ttl_minutes": 0, "remaining_seconds": None, "expires_at": None}
        remaining = max(0, int(round(self._deadline_mono - time.monotonic())))
        return {
            "enabled": True,
            "ttl_minutes": self._ttl_minutes,
            "remaining_seconds": remaining,
            "expires_at": self._expires_at,
        }

    def status(self):
        with self._lock:
            return self.status_locked()


AUTO_SHUTDOWN: AutoShutdown | None = None


def _same_origin(handler) -> bool:
    origin = handler.headers.get("Origin")
    if not origin:
        return True
    host = handler.headers.get("Host", "")
    parsed = urllib.parse.urlsplit(origin)
    return parsed.netloc == host


def overview() -> dict:
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        jobs = {
            "accounts": pool.submit(list_accounts),
            "compose": pool.submit(collect_compose),
            "host": pool.submit(HOST_COLLECTOR.collect),
        }
        result = {name: job.result() for name, job in jobs.items()}
    result["auto_shutdown"] = AUTO_SHUTDOWN.status() if AUTO_SHUTDOWN else {"enabled": False}
    result["generated_at"] = now_iso()
    return result


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = "duaa-dashboard/3.1"

    def _headers(self, content_type, length, cache="no-store"):
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(length))
        self.send_header("Cache-Control", cache)
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "SAMEORIGIN")
        self.send_header("Referrer-Policy", "no-referrer")

    def _json(self, value, status=200):
        body = json.dumps(value, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self._headers("application/json; charset=utf-8", len(body))
        self.end_headers()
        self.wfile.write(body)

    def _read_json_body(self, limit=65536, allow_empty=False):
        raw_len = self.headers.get("Content-Length", "0")
        try:
            length = int(raw_len)
        except ValueError:
            raise ValueError("Content-Length 无效")
        if length == 0 and allow_empty:
            return {}
        if length <= 0 or length > limit:
            raise ValueError("请求体大小无效")
        try:
            return json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise ValueError("请求体必须是有效 JSON")

    def _static(self, name):
        files = {"index.html": "index.html", "app.js": "app.js", "style.css": "style.css"}
        if name not in files:
            self.send_error(404)
            return
        path = STATIC / files[name]
        try:
            body = path.read_bytes()
        except OSError:
            self.send_error(404)
            return
        content_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        if content_type.startswith("text/") or content_type == "application/javascript":
            content_type += "; charset=utf-8"
        self.send_response(200)
        self._headers(content_type, len(body))
        self.end_headers()
        self.wfile.write(body)

    def _qr(self, name):
        if not ACCOUNT_NAME.fullmatch(name):
            self.send_error(404)
            return
        port = account_port(name)
        if not port:
            self.send_error(404)
            return
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/qr.png", timeout=12) as response:
                body = response.read(20 * 1024 * 1024 + 1)
            if len(body) > 20 * 1024 * 1024 or not body.startswith(b"\x89PNG"):
                raise ValueError("provider did not return a valid PNG")
        except Exception:
            self.send_error(502, "登录画面暂时不可用")
            return
        self.send_response(200)
        self._headers("image/png", len(body))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        try:
            if path in ("/", "/index", "/index.html"):
                self._static("index.html")
            elif path in ("/app.js", "/style.css"):
                self._static(path.lstrip("/"))
            elif path == "/healthz":
                self._json(
                    {
                        "ok": True,
                        "time": now_iso(),
                        "auto_shutdown": AUTO_SHUTDOWN.status() if AUTO_SHUTDOWN else {"enabled": False},
                    }
                )
            elif path == "/api/accounts":
                self._json(list_accounts())
            elif path == "/api/config":
                self._json(public_config())
            elif path == "/api/overview":
                self._json(overview())
            elif path == "/api/logs":
                query = urllib.parse.parse_qs(parsed.query)
                try:
                    lines = int(query.get("lines", ["200"])[0])
                except ValueError:
                    self._json({"error": "lines 必须是整数"}, 400)
                    return
                self._json(collect_logs(lines))
            else:
                match = re.fullmatch(r"/qr/([a-z0-9][a-z0-9_-]{0,23})\.png", path)
                if match:
                    self._qr(match.group(1))
                else:
                    self.send_error(404)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:
            if path.startswith("/api/"):
                self._json({"error": str(exc)[:300]}, 500)
            else:
                self.send_error(500)

    def do_PUT(self):
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path != "/api/config":
            self.send_error(404)
            return
        if not _same_origin(self):
            self._json({"error": "跨域请求被拒绝"}, 403)
            return
        try:
            before = public_config()
            payload = self._read_json_body(limit=CONFIG_BODY_LIMIT)
            data = validate_config(payload)
            restart_required = any(
                [
                    before["poll_interval_minutes"] != data["poll_interval_minutes"],
                    before["auto_window_minutes"] != data["auto_window_minutes"],
                    before["log_file"] != data["log_file"],
                ]
            )
            write_config(data)
        except (OSError, ValueError) as exc:
            self._json({"error": str(exc)[:400]}, 400)
            return
        self._json(
            {
                "saved": True,
                "path": CONFIG_PATH.name,
                "modified_at": config_mtime(),
                "hot_reload": not restart_required,
                "restart_required": restart_required,
            }
        )

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        if not _same_origin(self):
            self._json({"error": "跨域请求被拒绝"}, 403)
            return
        try:
            if path == "/api/accounts":
                payload = self._read_json_body()
                result = create_account(payload.get("name"), payload.get("http_port"))
                self._json(result, 201)
                return

            if path == "/api/duaa/restart":
                self._json(restart_duaa())
                return

            if path == "/api/dashboard/extend":
                payload = self._read_json_body(allow_empty=True)
                minutes = int(payload.get("minutes") or DEFAULT_TTL_MINUTES or 20)
                if not AUTO_SHUTDOWN:
                    self._json({"error": "未启用定时器"}, 400)
                    return
                self._json({"ok": True, "auto_shutdown": AUTO_SHUTDOWN.extend(minutes)})
                return

            if path == "/api/dashboard/shutdown":
                self._json({"ok": True, "stopping": True})
                if AUTO_SHUTDOWN:
                    AUTO_SHUTDOWN.shutdown_now()
                return

            match = re.fullmatch(r"/api/accounts/([a-z0-9][a-z0-9_-]{0,23})/(action|lifecycle|test-secret)", path)
            if not match:
                self.send_error(404)
                return
            name, kind = match.group(1), match.group(2)
            if kind == "action":
                payload = self._read_json_body()
                self._json(trigger_account_ui(name, str(payload.get("action", "")).strip()))
            elif kind == "lifecycle":
                payload = self._read_json_body()
                self._json(control_account(name, str(payload.get("op", "")).strip()))
            elif kind == "test-secret":
                self._json(test_account_secret(name))
        except ValueError as exc:
            self._json({"error": str(exc)[:300]}, 400)
        except LookupError as exc:
            self._json({"error": str(exc)[:300]}, 404)
        except urllib.error.HTTPError as exc:
            detail = exc.read(512).decode("utf-8", errors="replace")
            self._json({"error": f"Provider 返回 HTTP {exc.code}: {detail[:180]}"}, 502)
        except Exception as exc:
            self._json({"error": str(exc)[:300]}, 502)

    def do_DELETE(self):
        parsed = urllib.parse.urlparse(self.path)
        if not _same_origin(self):
            self._json({"error": "跨域请求被拒绝"}, 403)
            return
        match = re.fullmatch(r"/api/accounts/([a-z0-9][a-z0-9_-]{0,23})", parsed.path)
        if not match:
            self.send_error(404)
            return
        query = urllib.parse.parse_qs(parsed.query)
        remove_volume = query.get("remove_volume", ["0"])[0] in ("1", "true", "yes")
        try:
            self._json(delete_account(match.group(1), remove_volume=remove_volume))
        except ValueError as exc:
            self._json({"error": str(exc)[:300]}, 400)
        except LookupError as exc:
            self._json({"error": str(exc)[:300]}, 404)
        except Exception as exc:
            self._json({"error": str(exc)[:300]}, 502)

    def log_message(self, fmt, *args):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    httpd = Server((BIND, PORT), Handler)
    AUTO_SHUTDOWN = AutoShutdown(httpd, DEFAULT_TTL_MINUTES)
    ttl_msg = f"auto-shutdown in {DEFAULT_TTL_MINUTES}m" if DEFAULT_TTL_MINUTES > 0 else "auto-shutdown disabled"
    print(f"dashboard on http://{BIND}:{PORT} ({ttl_msg})", flush=True)
    try:
        httpd.serve_forever()
    finally:
        httpd.server_close()
