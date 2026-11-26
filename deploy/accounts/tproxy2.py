import http.server
import datetime
import json
import os
import random
import socket
import socketserver
import ssl as sslmod
import struct
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

LOG = os.environ.get("TPROXY_LOG", "/opt/duaa/log/tproxy.log")
LOG_MAX_BYTES = int(os.environ.get("TPROXY_LOG_MAX_BYTES", str(5 * 1024 * 1024)))
DNS_TTL = int(os.environ.get("TPROXY_DNS_TTL", "300"))
INTERCEPT_HOSTS = {"iclass.buaa.edu.cn", "open.weixin.qq.com", "qywx.buaa.edu.cn"}

# Optional manual override: TPROXY_UPSTREAM_MAP="host1=ip1,host2=ip2"
STATIC_MAP = {}
for item in os.environ.get("TPROXY_UPSTREAM_MAP", "").split(","):
    if "=" in item:
        h, ip = item.split("=", 1)
        if h.strip() and ip.strip():
            STATIC_MAP[h.strip().lower()] = ip.strip()

_DNS_CACHE = {}
_DNS_LOCK = threading.Lock()
_LOG_LOCK = threading.Lock()


def _nameservers():
    servers = []
    try:
        with open("/etc/resolv.conf", "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line.startswith("nameserver "):
                    ns = line.split()[1]
                    servers.append(ns)
    except OSError:
        pass
    for fallback in ("223.5.5.5", "114.114.114.114"):
        if fallback not in servers:
            servers.append(fallback)
    return servers


def _encode_qname(host):
    parts = [p.encode("ascii") for p in host.strip(".").split(".") if p]
    return b"".join(bytes([len(p)]) + p for p in parts) + b"\x00"


def _skip_name(buf, offset):
    while offset < len(buf):
        length = buf[offset]
        if length == 0:
            return offset + 1
        if (length & 0xC0) == 0xC0:
            return offset + 2
        offset += 1 + length
    return offset


def _udp_dns_a(host, nameserver, timeout=3.0):
    txid = random.randint(0, 65535)
    header = struct.pack("!HHHHHH", txid, 0x0100, 1, 0, 0, 0)
    question = _encode_qname(host) + struct.pack("!HH", 1, 1)  # Type A, Class IN
    packet = header + question
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(timeout)
    try:
        sock.sendto(packet, (nameserver, 53))
        resp, _ = sock.recvfrom(4096)
    finally:
        sock.close()
    if len(resp) < 12:
        return None
    r_txid, _flags, qdcount, ancount, _, _ = struct.unpack("!HHHHHH", resp[:12])
    if r_txid != txid or ancount == 0:
        return None
    offset = 12
    for _ in range(qdcount):
        offset = _skip_name(resp, offset) + 4
    for _ in range(ancount):
        offset = _skip_name(resp, offset)
        if offset + 10 > len(resp):
            break
        rtype, rclass, ttl, rdlength = struct.unpack("!HHIH", resp[offset : offset + 10])
        offset += 10
        rdata = resp[offset : offset + rdlength]
        offset += rdlength
        if rtype == 1 and rclass == 1 and rdlength == 4:
            ip = socket.inet_ntoa(rdata)
            if not ip.startswith("127."):
                return ip, max(30, min(ttl, DNS_TTL))
    return None


def resolve_upstream(host):
    key = host.lower()
    if key in STATIC_MAP:
        return STATIC_MAP[key]
    now = time.monotonic()
    with _DNS_LOCK:
        cached = _DNS_CACHE.get(key)
        if cached and cached[1] > now:
            return cached[0]
    for ns in _nameservers():
        try:
            ans = _udp_dns_a(key, ns)
            if ans:
                ip, ttl = ans
                with _DNS_LOCK:
                    _DNS_CACHE[key] = (ip, now + ttl)
                return ip
        except OSError:
            continue
    return None


ORIG_GAI = socket.getaddrinfo


def patched_getaddrinfo(host, port, *a, **k):
    if isinstance(host, str) and host.lower() in INTERCEPT_HOSTS:
        ip = resolve_upstream(host)
        if ip:
            host = ip
    return ORIG_GAI(host, port, *a, **k)


socket.getaddrinfo = patched_getaddrinfo

CTX = sslmod.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = sslmod.CERT_NONE


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


OPENER = urllib.request.build_opener(NoRedirect, urllib.request.HTTPSHandler(context=CTX))


def append_log(record):
    line = json.dumps(record, ensure_ascii=False) + "\n"
    with _LOG_LOCK:
        try:
            if os.path.exists(LOG) and os.path.getsize(LOG) > LOG_MAX_BYTES:
                with open(LOG, "rb") as rf:
                    rf.seek(max(0, os.path.getsize(LOG) - 512 * 1024))
                    tail = rf.read()
                nl = tail.find(b"\n")
                if nl >= 0:
                    tail = tail[nl + 1 :]
                with open(LOG, "wb") as wf:
                    wf.write(tail)
        except OSError:
            pass
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(line)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _handle(self):
        host = (self.headers.get("Host") or "iclass.buaa.edu.cn").split(":")[0]
        port = self.server.server_address[1]
        body = None
        if self.command == "POST":
            body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        scheme = "http" if port in (8081, 8082) else "https"
        url = f"{scheme}://{host}:{port}{self.path}"
        req = urllib.request.Request(url, data=body, method=self.command)
        for k in ("User-Agent", "Cookie", "Content-Type", "Accept", "Referer", "Origin"):
            if self.headers.get(k):
                req.add_header(k, self.headers[k])
        st = 502
        hs = {}
        out = b"proxy error"
        first = ""
        try:
            try:
                resp = OPENER.open(req, timeout=20)
                out = resp.read()
                st = resp.status
                hs = dict(resp.getheaders())
            except urllib.error.HTTPError as e:
                out = e.read()
                st = e.code
                hs = dict(e.headers.items())
            ct = str(hs.get("Content-Type", ""))
            if "html" in ct.lower() or "json" in ct.lower():
                first = out[:500].decode("utf-8", "replace")
        except Exception as e:
            out = f"proxy error: {e}".encode()
            hs = {"Content-Type": "text/plain"}
        code = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get("code", [None])[0]
        append_log(
            {
                "t": datetime.datetime.now().isoformat(),
                "host": host,
                "port": port,
                "m": self.command,
                "path": self.path,
                "status": st,
                "code": code,
                "head": first,
            }
        )
        self.send_response(st)
        for k, v in hs.items():
            if k.lower() in ("transfer-encoding", "content-encoding", "connection"):
                continue
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    do_GET = _handle
    do_POST = _handle
    do_HEAD = _handle

    def log_message(self, *a):
        pass


class ThreadedServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    for p in (8346, 443, 8188, 8081, 8082):
        srv = ThreadedServer(("127.0.0.1", p), Handler)
        if p not in (8081, 8082):
            c = sslmod.SSLContext(sslmod.PROTOCOL_TLS_SERVER)
            base = os.environ.get("TPROXY_CERT_DIR", "/opt/duaa/run")
            c.load_cert_chain(f"{base}/srv.crt", f"{base}/srv.key")
            srv.socket = c.wrap_socket(srv.socket, server_side=True)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        print("tproxy2", p, flush=True)
    while True:
        time.sleep(3600)
