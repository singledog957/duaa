FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY . /app/deploy/dashboard

CMD ["python3", "deploy/dashboard/server.py"]
