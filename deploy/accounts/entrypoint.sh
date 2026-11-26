#!/bin/bash
# 单账号容器入口：CA → tproxy → Xvfb → wwlocal → provider
set -e
mkdir -p /opt/duaa/run /opt/duaa/log

# 1. 每容器独立 CA + 服务证书（SAN 覆盖三个域名），装入系统与 NSS。
if [ ! -f /opt/duaa/run/ca.crt ]; then
  cd /opt/duaa/run
  openssl req -x509 -newkey rsa:2048 -keyout ca.key -out ca.crt \
    -days 3650 -nodes -subj '/CN=DUAA Account CA' >/dev/null 2>&1
  openssl req -newkey rsa:2048 -keyout srv.key -out srv.csr -nodes \
    -subj '/CN=iclass.buaa.edu.cn' >/dev/null 2>&1
  printf 'subjectAltName=DNS:iclass.buaa.edu.cn,DNS:open.weixin.qq.com,DNS:qywx.buaa.edu.cn\n' > san.ext
  openssl x509 -req -in srv.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    -out srv.crt -days 3650 -extfile san.ext >/dev/null 2>&1
  cp ca.crt /usr/local/share/ca-certificates/duaa-account-ca.crt
  update-ca-certificates >/dev/null 2>&1
fi
mkdir -p /root/.pki
[ -f /root/.pki/nssdb/pkcs11.txt ] || cp -r /opt/duaa/nssdb-template /root/.pki/nssdb
certutil -A -d sql:/root/.pki/nssdb -n 'DUAA Account CA' -t 'CT,C,T' \
  -i /opt/duaa/run/ca.crt 2>/dev/null || true

# 2. 透明代理（443/8188/8346 TLS + 8081/8082 明文，转发真实服务器）。
python3 /opt/duaa/tproxy2.py >> /opt/duaa/log/tproxy.log 2>&1 &
sleep 1

# 3. Xvfb（无头显示，供截图、OCR 与自动点击）。
export DISPLAY_NUM=99
export DISPLAY=:${DISPLAY_NUM}
for _ in $(seq 1 5); do
  rm -f /tmp/.X11-unix/X${DISPLAY_NUM} 2>/dev/null || true
  Xvfb :${DISPLAY_NUM} -screen 0 1920x1080x24 -nolisten tcp \
    >>/opt/duaa/log/xvfb.log 2>&1 &
  sleep 1
  [ -S /tmp/.X11-unix/X${DISPLAY_NUM} ] && break
  DISPLAY_NUM=$((DISPLAY_NUM+1))
  export DISPLAY=:${DISPLAY_NUM}
  pkill -x Xvfb 2>/dev/null || true
done
sleep 1

# 4. wwlocal（登录态在 /root，由 volume 持久化）。
/opt/i北航/wwlocal >> /opt/duaa/log/wwlocal.log 2>&1 &
/opt/duaa/clicklib.sh auto-qr >> /opt/duaa/log/clicklib.dbg 2>&1 &

# 5. provider（默认 9000：/health /secret /qr.png）。
exec python3 /opt/duaa/provider.py
