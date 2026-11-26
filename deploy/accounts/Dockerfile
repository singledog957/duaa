FROM ubuntu:22.04

ARG APT_MIRROR=mirrors.ustc.edu.cn
ENV DEBIAN_FRONTEND=noninteractive
# wwlocal 安装路径含中文（/opt/i北航），Qt 在非 UTF-8 locale 下会截断路径。
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

COPY weworklocal*.deb /tmp/

RUN if [ -n "$APT_MIRROR" ]; then \
      sed -i "s|archive.ubuntu.com|$APT_MIRROR|g; s|security.ubuntu.com|$APT_MIRROR|g" /etc/apt/sources.list; \
    fi \
    && apt-get update && apt-get install -y --no-install-recommends \
      xvfb x11-utils imagemagick curl ca-certificates openssl \
      libx11-6 libxcb-xinerama0 fonts-noto fonts-noto-cjk \
      libnss3-tools zbar-tools python3 python3-pip xdotool \
      libxkbcommon-x11-0 libxkbcommon0 libxcb-cursor0 libxcb-icccm4 \
      libxcb-keysyms1 libxcb-shape0 \
      libasound2 libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
      libatomic1 \
      libatk1.0-0 libatk-bridge2.0-0 libatspi2.0-0 libgtk-3-0 \
      libnss3 libnspr4 libdrm2 libgbm1 libxss1 libxcursor1 libxrandr2 \
      libxcomposite1 libxdamage1 libxfixes3 libcups2 libpango-1.0-0 libcairo2 \
      tesseract-ocr tesseract-ocr-chi-sim \
    && mkdir -p /home/桌面 \
    && (dpkg -i /tmp/weworklocal*.deb || apt-get -f install -y) \
    && dpkg -s wwlocal >/dev/null \
    && rm -f /tmp/weworklocal*.deb \
    && rm -rf /var/lib/apt/lists/*

# 保持与实测环境一致的 wrapper 参数
RUN set -e; WX=/opt/i北航/RadiumWMPF/runtime; \
    mv "$WX/WeChatAppEx" "$WX/WeChatAppEx.real" && \
    printf '#!/bin/sh\nexec "$(dirname "$0")/WeChatAppEx.real" "$@" --enable-ui-devtools\n' \
      > "$WX/WeChatAppEx" && chmod +x "$WX/WeChatAppEx"

COPY tproxy2.py /opt/duaa/tproxy2.py
COPY clicklib.sh /opt/duaa/clicklib.sh
COPY provider.py /opt/duaa/provider.py
COPY entrypoint.sh /entrypoint.sh

# NSS DB 模板：构建期生成，运行期直接复制避免 certutil 偶发挂起。
RUN chmod +x /entrypoint.sh /opt/duaa/clicklib.sh /opt/duaa/tproxy2.py /opt/duaa/provider.py \
    && mkdir -p /opt/duaa/nssdb-template \
    && cd /opt/duaa/nssdb-template \
    && timeout 15 certutil -N -d sql:. --empty-password >/dev/null 2>&1 || true \
    && test -f pkcs11.txt && test -f cert9.db && test -f key4.db

EXPOSE 9000
ENTRYPOINT ["/entrypoint.sh"]
