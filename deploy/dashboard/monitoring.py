"""Host metrics, Docker Compose status, and safe log collectors for duaa dashboard."""

from __future__ import annotations

import datetime as dt
import http.client
import json
import os
import shutil
import socket
import threading
import time
from pathlib import Path


DEPLOY_ROOT = Path(__file__).resolve().parent.parent
PROJECT_ROOT = DEPLOY_ROOT.parent
INSTANCES = DEPLOY_ROOT / "accounts" / "instances"
LOGS_DIR = (PROJECT_ROOT / "logs").resolve()
ROOT_COMPOSE_FILE = DEPLOY_ROOT / "docker-compose.yml"
LOG_LINES_MAX = 500
DOCKER_SOCKET = os.environ.get("DOCKER_SOCKET", "/var/run/docker.sock")
HOST_PROC = os.environ.get("HOST_PROC", "/proc")
HOST_SYS = os.environ.get("HOST_SYS", "/sys")


def now_iso():
    return dt.datetime.now().astimezone().isoformat(timespec="seconds")


class _UnixHTTPConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout=10):
        super().__init__("localhost", timeout=timeout)
        self._unix_path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self._unix_path)


def docker_request(method, path, payload=None, timeout=15):
    """Send a request to the Docker Engine API over Unix socket."""
    connection = _UnixHTTPConnection(DOCKER_SOCKET, timeout=timeout)
    headers = {}
    body = None
    if payload is not None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        raw = response.read(16 * 1024 * 1024)
        status = response.status
    except OSError as exc:
        return 0, {"message": f"Docker API 连接失败: {exc}"}
    finally:
        connection.close()

    if not raw:
        return status, {}
    try:
        return status, json.loads(raw.decode("utf-8", errors="replace"))
    except ValueError:
        return status, {"message": raw.decode("utf-8", errors="replace")[:400]}


def list_docker_containers():
    status, data = docker_request("GET", "/v1.41/containers/json?all=1", timeout=6)
    if status != 200 or not isinstance(data, list):
        msg = data.get("message") if isinstance(data, dict) else f"HTTP {status}"
        return [], f"Docker API 查询失败: {msg}"
    containers = []
    for row in data:
        labels = row.get("Labels") or {}
        ports = []
        for binding in row.get("Ports") or []:
            public = binding.get("PublicPort")
            private = binding.get("PrivatePort")
            if public and private:
                ip = binding.get("IP") or ""
                prefix = f"{ip}:" if ip == "127.0.0.1" else ""
                mapping = f"{prefix}{public}->{private}"
                if mapping not in ports:
                    ports.append(mapping)
        containers.append(
            {
                "id": row.get("Id", "")[:12],
                "name": (row.get("Names") or [""])[0].lstrip("/"),
                "service": labels.get("com.docker.compose.service", ""),
                "project": labels.get("com.docker.compose.project", ""),
                "config_file": labels.get("com.docker.compose.project.config_files", ""),
                "state": row.get("State", ""),
                "status": row.get("Status", ""),
                "image": row.get("Image", ""),
                "ports": ", ".join(ports),
            }
        )
    return containers, ""


def collect_compose():
    compose_files = [ROOT_COMPOSE_FILE]
    if INSTANCES.is_dir():
        compose_files.extend(
            path / "compose.yml" for path in INSTANCES.iterdir() if path.is_dir()
        )
    allowed_files = {
        path.relative_to(PROJECT_ROOT).as_posix()
        for path in compose_files
        if path.is_file()
    }
    all_containers, error = list_docker_containers()
    containers = [
        item
        for item in all_containers
        if item["name"] in ("duaa", "duaa-dashboard")
        or item["name"].startswith("duaa-acc-")
        or any(item["config_file"].endswith(suffix) for suffix in allowed_files)
    ]
    containers.sort(key=lambda item: (item["project"], item["service"], item["name"]))
    return {
        "ok": not error,
        "error": error,
        "containers": containers,
        "counts": {
            "total": len(containers),
            "running": sum(item["state"].lower() == "running" for item in containers),
            "stopped": sum(item["state"].lower() != "running" for item in containers),
        },
    }


class HostCollector:
    def __init__(self):
        self._lock = threading.Lock()
        self._last_cpu = None
        self._last_network = None

    @staticmethod
    def _cpu_times():
        values = [int(value) for value in Path(HOST_PROC, "stat").read_text().splitlines()[0].split()[1:]]
        idle = values[3] + (values[4] if len(values) > 4 else 0)
        return sum(values), idle

    def _cpu_percent(self):
        current = self._cpu_times()
        previous = self._last_cpu
        self._last_cpu = current
        if previous is None:
            return None
        total_delta = current[0] - previous[0]
        idle_delta = current[1] - previous[1]
        if total_delta <= 0:
            return 0.0
        return round(max(0.0, min(100.0, 100 * (1 - idle_delta / total_delta))), 1)

    def _network(self):
        received = sent = 0
        for line in Path(HOST_PROC, "net/dev").read_text().splitlines()[2:]:
            interface, values = line.split(":", 1)
            if interface.strip() == "lo":
                continue
            fields = values.split()
            received += int(fields[0])
            sent += int(fields[8])
        current = (time.monotonic(), received, sent)
        previous = self._last_network
        self._last_network = current
        rx_rate = tx_rate = None
        if previous and current[0] > previous[0]:
            elapsed = current[0] - previous[0]
            rx_rate = max(0, round((received - previous[1]) / elapsed, 1))
            tx_rate = max(0, round((sent - previous[2]) / elapsed, 1))
        return {
            "received_bytes": received,
            "sent_bytes": sent,
            "receive_bytes_per_second": rx_rate,
            "send_bytes_per_second": tx_rate,
        }

    @staticmethod
    def _memory():
        values = {}
        for line in Path(HOST_PROC, "meminfo").read_text().splitlines():
            key, value = line.split(":", 1)
            values[key] = int(value.strip().split()[0]) * 1024
        total = values.get("MemTotal", 0)
        available = values.get("MemAvailable", values.get("MemFree", 0))
        swap_total = values.get("SwapTotal", 0)
        swap_free = values.get("SwapFree", 0)
        return {
            "total_bytes": total,
            "available_bytes": available,
            "used_bytes": max(0, total - available),
            "used_percent": round(100 * (total - available) / total, 1) if total else 0,
            "swap_total_bytes": swap_total,
            "swap_used_bytes": max(0, swap_total - swap_free),
        }

    @staticmethod
    def _disks():
        disks = []
        seen_devices = set()
        for path in ("/", "/app"):
            if not os.path.exists(path):
                continue
            try:
                device = os.stat(path).st_dev
                if device in seen_devices:
                    continue
                seen_devices.add(device)
                usage = shutil.disk_usage(path)
                disks.append(
                    {
                        "path": path,
                        "total_bytes": usage.total,
                        "used_bytes": usage.used,
                        "free_bytes": usage.free,
                        "used_percent": round(100 * usage.used / usage.total, 1) if usage.total else 0,
                    }
                )
            except OSError:
                continue
        return disks

    @staticmethod
    def _temperatures():
        temperatures = []
        for zone in Path(HOST_SYS, "class/thermal").glob("thermal_zone*"):
            try:
                value = int((zone / "temp").read_text().strip()) / 1000
                label = (zone / "type").read_text().strip()
                temperatures.append({"name": label, "celsius": round(value, 1)})
            except (OSError, ValueError):
                continue
        return temperatures

    def collect(self):
        with self._lock:
            try:
                load = Path(HOST_PROC, "loadavg").read_text().split()
                uptime = float(Path(HOST_PROC, "uptime").read_text().split()[0])
                pretty_name = "Linux"
                release = Path("/etc/os-release")
                if release.is_file():
                    for line in release.read_text().splitlines():
                        if line.startswith("PRETTY_NAME="):
                            pretty_name = line.split("=", 1)[1].strip().strip('"')
                            break
                return {
                    "ok": True,
                    "hostname": socket.gethostname(),
                    "os": pretty_name,
                    "kernel": os.uname().release,
                    "architecture": os.uname().machine,
                    "cpu_count": os.cpu_count(),
                    "cpu_percent": self._cpu_percent(),
                    "load_average": [float(value) for value in load[:3]],
                    "uptime_seconds": int(uptime),
                    "memory": self._memory(),
                    "disks": self._disks(),
                    "network": self._network(),
                    "temperatures": self._temperatures(),
                }
            except (OSError, ValueError, IndexError) as exc:
                return {"ok": False, "hostname": socket.gethostname(), "error": str(exc)}


HOST_COLLECTOR = HostCollector()


def resolve_safe_log_path(raw_value: str | None) -> Path | None:
    """Resolve a configured log path strictly inside PROJECT_ROOT/logs/."""
    if not raw_value:
        return None
    val = str(raw_value).strip()
    if not val:
        return None
    if val.startswith("/app/"):
        candidate = PROJECT_ROOT / val[len("/app/"):]
    elif Path(val).is_absolute():
        candidate = Path(val)
    else:
        candidate = PROJECT_ROOT / val
    try:
        resolved = candidate.resolve()
        resolved.relative_to(LOGS_DIR)
        return resolved
    except (OSError, ValueError):
        return None


def _configured_log_path() -> Path:
    configured = os.environ.get("DASHBOARD_DUAA_LOG")
    if configured:
        safe = resolve_safe_log_path(configured)
        if safe:
            return safe
    config_env = os.environ.get("DASHBOARD_DUAA_CONFIG", "config.json")
    config_path = Path(config_env) if Path(config_env).is_absolute() else PROJECT_ROOT / config_env
    if config_path.is_file():
        try:
            value = json.loads(config_path.read_text(encoding="utf-8")).get("log_file")
            safe = resolve_safe_log_path(value)
            if safe:
                return safe
        except (OSError, json.JSONDecodeError):
            pass
    return LOGS_DIR / "duaa.log"


def collect_logs(lines=200):
    lines = max(20, min(LOG_LINES_MAX, int(lines)))
    path = _configured_log_path()
    result = {
        "source": "duaa",
        "name": f"logs/{path.name}",
        "lines": [],
        "error": "",
        "read_at": now_iso(),
        "size_bytes": 0,
        "modified_at": None,
    }
    try:
        stat = path.stat()
        result["size_bytes"] = stat.st_size
        result["modified_at"] = dt.datetime.fromtimestamp(stat.st_mtime).astimezone().isoformat(
            timespec="seconds"
        )
        with path.open("rb") as handle:
            handle.seek(max(0, stat.st_size - 1024 * 1024))
            content = handle.read().decode("utf-8", errors="replace")
        result["lines"] = content.splitlines()[-lines:]
    except OSError as exc:
        result["error"] = f"日志文件不可读：{exc.strerror or exc}"
    return result
