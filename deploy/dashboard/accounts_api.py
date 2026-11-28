"""8081 account container lifecycle management via Docker Engine Unix socket API."""

from __future__ import annotations

import json
import os
import re
import shutil
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from monitoring import INSTANCES, PROJECT_ROOT, docker_request, list_docker_containers, now_iso

ACCOUNT_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,23}$")
RESERVED_NAMES = {"duaa", "dashboard"}
ACCOUNT_IMAGE = "duaa-account:latest"
ALLOWED_UI_ACTIONS = {"login", "qr"}
ALLOWED_LIFECYCLE_OPS = {"start", "stop", "restart"}


def _host_project_root() -> str:
    """Discover host-side project directory for accurate Compose labels."""
    status, data = docker_request("GET", "/v1.41/containers/duaa-dashboard/json", timeout=4)
    if status == 200 and isinstance(data, dict):
        for mount in data.get("Mounts") or []:
            if mount.get("Destination") == "/app" and mount.get("Source"):
                return str(mount["Source"])
    return str(PROJECT_ROOT)


def _chown_like_project(path: Path):
    try:
        st = PROJECT_ROOT.stat()
        os.chown(path, st.st_uid, st.st_gid)
    except OSError:
        pass


def _parse_port(compose_file: Path) -> int | None:
    if not compose_file.is_file():
        return None
    text = compose_file.read_text(encoding="utf-8")
    m = re.search(r"(\d+):9000", text)
    return int(m.group(1)) if m else None


def account_port(name: str) -> int | None:
    if not name or not ACCOUNT_NAME_RE.fullmatch(name):
        return None
    directory = INSTANCES / name
    if not directory.is_dir():
        return None
    return _parse_port(directory / "compose.yml")


def next_available_port() -> int:
    used = set()
    if INSTANCES.is_dir():
        for path in INSTANCES.iterdir():
            if path.is_dir():
                p = _parse_port(path / "compose.yml")
                if p:
                    used.add(p)
    port = 9101
    while port in used:
        port += 1
    return port


def _probe_account(directory: Path, container_map: dict) -> dict | None:
    compose_file = directory / "compose.yml"
    if not compose_file.is_file():
        return None
    name = directory.name
    port = _parse_port(compose_file)
    cname = f"duaa-acc-{name}"
    cinfo = container_map.get(cname) or {}
    cstate = (cinfo.get("state") or "missing").lower()
    entry = {
        "name": name,
        "mode": "compose",
        "container_name": cname,
        "container_state": cstate,
        "container_status": cinfo.get("status") or ("未创建" if cstate == "missing" else cstate),
        "http_port": port,
        "secret_url": f"http://127.0.0.1:{port}/secret" if port else "",
        "health": {"ok": False, "error": "容器未运行" if cstate != "running" else "未配置 provider 端口"},
        "checked_at": now_iso(),
    }
    if not port or cstate != "running":
        return entry
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=3) as resp:
            health = json.loads(resp.read(65536).decode("utf-8"))
            if isinstance(health, dict):
                entry["health"] = health
    except Exception as exc:
        entry["health"] = {"ok": False, "error": str(exc)[:180]}
    return entry


def list_accounts() -> list[dict]:
    if not INSTANCES.is_dir():
        return []
    directories = sorted(path for path in INSTANCES.iterdir() if path.is_dir())
    if not directories:
        return []
    containers, _ = list_docker_containers()
    cmap = {c["name"]: c for c in containers}
    with ThreadPoolExecutor(max_workers=min(8, len(directories))) as pool:
        entries = list(pool.map(lambda d: _probe_account(d, cmap), directories))
    return [entry for entry in entries if entry]


def _write_compose_file(name: str, port: int) -> Path:
    INSTANCES.mkdir(parents=True, exist_ok=True)
    _chown_like_project(INSTANCES)
    directory = INSTANCES / name
    directory.mkdir(parents=True, exist_ok=True)
    _chown_like_project(directory)
    compose_file = directory / "compose.yml"
    compose_file.write_text(
        f"""services:
  wwlocal:
    image: {ACCOUNT_IMAGE}
    build:
      context: ../..
    container_name: duaa-acc-{name}
    hostname: duaa-acc-{name}
    extra_hosts:
      - "iclass.buaa.edu.cn:127.0.0.1"
      - "open.weixin.qq.com:127.0.0.1"
    ports:
      - "127.0.0.1:{port}:9000"
    volumes:
      - duaa-acc-{name}-data:/root
    restart: unless-stopped

volumes:
  duaa-acc-{name}-data:
    name: duaa-acc-{name}-data
""",
        encoding="utf-8",
    )
    _chown_like_project(compose_file)
    return compose_file


def create_account(name: str, port=None) -> dict:
    name = str(name or "").strip().lower()
    if not ACCOUNT_NAME_RE.fullmatch(name) or name in RESERVED_NAMES:
        raise ValueError("实例名不合法（仅允许小写字母、数字、-、_，长度 1-24，且不能为 duaa/dashboard）")
    if (INSTANCES / name / "compose.yml").is_file():
        raise ValueError(f"账号实例 {name} 已存在")

    if port is None or str(port).strip() == "":
        http_port = next_available_port()
    else:
        try:
            http_port = int(port)
        except (TypeError, ValueError):
            raise ValueError("端口必须是整数")
        if not (1024 <= http_port <= 65535):
            raise ValueError("端口必须在 1024-65535 之间")
        for existing in list_accounts():
            if existing["http_port"] == http_port:
                raise ValueError(f"端口 {http_port} 已被实例 {existing['name']} 占用")

    # Verify base image exists
    img_status, _ = docker_request("GET", f"/v1.41/images/{ACCOUNT_IMAGE}/json", timeout=5)
    if img_status != 200:
        raise RuntimeError(
            "未检测到 duaa-account:latest 基础镜像。请先从 https://qywx.buaa.edu.cn/ 下载官方 Linux 客户端放到 deploy/accounts/ 目录，并运行 bash deploy/manage.sh build"
        )

    _write_compose_file(name, http_port)
    cname = f"duaa-acc-{name}"
    vol_name = f"duaa-acc-{name}-data"
    host_root = _host_project_root()

    # Create persistent login volume
    v_status, v_data = docker_request("POST", "/v1.41/volumes/create", {"Name": vol_name}, timeout=10)
    if v_status not in (200, 201):
        raise RuntimeError(f"创建数据卷失败：{v_data.get('message', v_status)}")

    # Create container matching compose.yml 1:1
    container_spec = {
        "Image": ACCOUNT_IMAGE,
        "Hostname": cname,
        "ExposedPorts": {"9000/tcp": {}},
        "Labels": {
            "com.docker.compose.project": name,
            "com.docker.compose.service": "wwlocal",
            "com.docker.compose.project.working_dir": f"{host_root}/deploy/accounts/instances/{name}",
            "com.docker.compose.project.config_files": f"{host_root}/deploy/accounts/instances/{name}/compose.yml",
        },
        "HostConfig": {
            "ExtraHosts": [
                "iclass.buaa.edu.cn:127.0.0.1",
                "open.weixin.qq.com:127.0.0.1",
            ],
            "Binds": [f"{vol_name}:/root"],
            "PortBindings": {
                "9000/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(http_port)}]
            },
            "RestartPolicy": {"Name": "unless-stopped"},
        },
    }
    c_status, c_data = docker_request(
        "POST", f"/v1.41/containers/create?name={cname}", container_spec, timeout=15
    )
    if c_status not in (200, 201):
        shutil.rmtree(INSTANCES / name, ignore_errors=True)
        raise RuntimeError(f"创建容器失败：{c_data.get('message', c_status)}")

    s_status, s_data = docker_request("POST", f"/v1.41/containers/{cname}/start", timeout=15)
    if s_status not in (200, 204, 304):
        raise RuntimeError(f"启动容器失败：{s_data.get('message', s_status)}")

    return {
        "ok": True,
        "name": name,
        "container_name": cname,
        "http_port": http_port,
        "secret_url": f"http://127.0.0.1:{http_port}/secret",
    }


def control_account(name: str, op: str) -> dict:
    if not ACCOUNT_NAME_RE.fullmatch(name or "") or not (INSTANCES / name / "compose.yml").is_file():
        raise LookupError(f"未找到账号实例：{name}")
    if op not in ALLOWED_LIFECYCLE_OPS:
        raise ValueError("不支持的容器操作")
    cname = f"duaa-acc-{name}"
    status, data = docker_request("POST", f"/v1.41/containers/{cname}/{op}", timeout=30)
    if status not in (200, 204, 304):
        raise RuntimeError(f"容器 {op} 失败：{data.get('message', status)}")
    return {"ok": True, "name": name, "op": op}


def delete_account(name: str, remove_volume: bool = False) -> dict:
    if not ACCOUNT_NAME_RE.fullmatch(name or "") or name in RESERVED_NAMES:
        raise ValueError("实例名不合法")
    directory = INSTANCES / name
    if not directory.is_dir():
        raise LookupError(f"未找到账号实例：{name}")
    cname = f"duaa-acc-{name}"
    vol_name = f"duaa-acc-{name}-data"

    # Remove container (ignore 404 if already gone)
    status, data = docker_request("DELETE", f"/v1.41/containers/{cname}?force=1", timeout=25)
    if status not in (200, 204, 404):
        raise RuntimeError(f"删除容器失败：{data.get('message', status)}")

    shutil.rmtree(directory, ignore_errors=True)

    volume_removed = False
    if remove_volume:
        v_status, _ = docker_request("DELETE", f"/v1.41/volumes/{vol_name}?force=1", timeout=10)
        volume_removed = v_status in (200, 204, 404)

    return {"ok": True, "name": name, "volume_removed": volume_removed}


def trigger_account_ui(name: str, action: str) -> dict:
    if action not in ALLOWED_UI_ACTIONS:
        raise ValueError("不支持的操作")
    port = account_port(name)
    if not port:
        raise LookupError(f"未找到账号实例：{name}")
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/action/{action}",
        data=b"",
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read(65536).decode("utf-8"))


def test_account_secret(name: str) -> dict:
    port = account_port(name)
    if not port:
        raise LookupError(f"未找到账号实例：{name}")
    start = time.monotonic()
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/secret", timeout=45) as resp:
        data = json.loads(resp.read(65536).decode("utf-8"))
    elapsed = round(time.monotonic() - start, 2)
    taskid = str(data.get("taskid", ""))
    secret = str(data.get("signSecret", ""))
    return {
        "ok": bool(taskid and secret),
        "elapsed_seconds": elapsed,
        "taskid_preview": f"{taskid[:8]}…" if taskid else "",
        "secret_preview": f"{secret[:6]}…" if secret else "",
        "login_name": data.get("loginName", ""),
    }


def restart_duaa() -> dict:
    status, data = docker_request("POST", "/v1.41/containers/duaa/restart", timeout=30)
    if status not in (200, 204, 304):
        raise RuntimeError(f"重启 duaa 失败：{data.get('message', status)}")
    return {"ok": True, "service": "duaa"}
