#!/usr/bin/env python3
import http.server
import json
import os
import signal
import socketserver
import subprocess
import threading
import urllib.parse

SECRET_LOCK = threading.Lock()
SECRET_TIMEOUT = int(os.environ.get("SECRET_TIMEOUT", "40"))
ACTION_COMMANDS = {
    "login": "login-click",
    "qr": "show-qr",
}


def alive(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True).returncode == 0


def port_open(port):
    import socket
    s = socket.socket()
    s.settimeout(1)
    ok = s.connect_ex(("127.0.0.1", port)) == 0
    s.close()
    return ok


def run_clicklib(subcmd, timeout):
    proc = subprocess.Popen(
        ["/opt/duaa/clicklib.sh", subcmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
        return proc.returncode, stdout, stderr
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.communicate()
        raise


class Handler(http.server.BaseHTTPRequestHandler):
    def _json(self, obj, code=200):
        body = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = urllib.parse.urlparse(self.path).path
        if path == "/health":
            self._json({
                "ok": True,
                "wwlocal": alive("pgrep -x wwlocal"),
                "renderer": alive("pgrep -f wmpf-render-type=0"),
                "tproxy": port_open(8346),
            })
        elif path == "/secret":
            if not SECRET_LOCK.acquire(blocking=False):
                self._json({"error": "secret_in_progress"}, 503)
                return
            try:
                rc, stdout, stderr = run_clicklib("secret", SECRET_TIMEOUT)
                kv = dict(
                    l.split("=", 1)
                    for l in stdout.splitlines() if "=" in l
                )
                if rc != 0 or not kv.get("TASKID"):
                    self._json({
                        "error": "not_logged_in_or_ui_off",
                        "stderr": stderr[-400:],
                    }, 503)
                    return
                self._json({
                    "taskid": kv["TASKID"],
                    "signSecret": kv["SECRET"],
                    "loginName": kv.get("LOGIN_NAME", ""),
                })
            except subprocess.TimeoutExpired:
                self._json({"error": "timeout"}, 504)
            finally:
                SECRET_LOCK.release()
        elif path == "/qr.png":
            png = subprocess.run(
                ["import", "-window", "root", "png:-"],
                capture_output=True,
            ).stdout
            self.send_response(200)
            self.send_header("Content-Type", "image/png")
            self.send_header("Content-Length", str(len(png)))
            self.end_headers()
            self.wfile.write(png)
        else:
            self._json({"error": "not_found"}, 404)

    def do_POST(self):
        path = urllib.parse.urlparse(self.path).path
        if not path.startswith("/action/"):
            self._json({"error": "not_found"}, 404)
            return
        action = path[len("/action/"):]
        subcmd = ACTION_COMMANDS.get(action)
        if not subcmd:
            self._json({"error": "unsupported_action"}, 400)
            return
        if not SECRET_LOCK.acquire(timeout=5):
            self._json({"error": "busy"}, 503)
            return
        try:
            rc, stdout, stderr = run_clicklib(subcmd, 15)
            kv = dict(
                l.split("=", 1)
                for l in stdout.splitlines() if "=" in l
            )
            self._json({
                "ok": rc == 0,
                "action": action,
                "detail": kv.get("ACTION", ""),
                "stderr": stderr[-200:] if rc != 0 else "",
            }, 200 if rc == 0 else 500)
        except subprocess.TimeoutExpired:
            self._json({"error": "timeout"}, 504)
        finally:
            SECRET_LOCK.release()

    def log_message(self, *a):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    port = int(os.environ.get("PROVIDER_PORT", "9000"))
    Server(("0.0.0.0", port), Handler).serve_forever()
