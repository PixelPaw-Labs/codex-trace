#!/bin/bash
# Build the frontend, rebuild the Docker image, and restart via docker-compose.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

container_ids_publishing_port() {
  local port="$1"
  docker ps --format '{{.ID}} {{.Ports}}' |
    awk -v published=":${port}->" 'index($0, published) { print $1 }'
}

main() {
  cd "$ROOT_DIR"

  echo "==> Installing dependencies..."
  npm ci

  echo "==> Building frontend..."
  npm run build

  echo "==> Stopping existing containers..."
  docker compose down --remove-orphans 2>/dev/null || true
  # Also stop any manually-started container occupying the target port
  local port="${CODEXTRACE_HOST_PORT:-1422}"
  local existing
  existing=$(container_ids_publishing_port "$port")
  if [ -n "$existing" ]; then
    echo "    Stopping container(s) on port ${port}: $existing"
    docker stop $existing >/dev/null
    docker rm $existing >/dev/null 2>&1 || true
  fi

  echo "==> Building Docker image (no cache on --fresh)..."
  if [[ "${1:-}" == "--fresh" ]]; then
    docker compose build --no-cache
  else
    docker compose build
  fi

  echo "==> Starting container..."
  docker compose up -d

  echo "==> Waiting for service to be ready..."
  local attempt
  for attempt in $(seq 1 20); do
    if curl -sf "http://localhost:${port}/" >/dev/null 2>&1; then
      echo "==> Service is up."
      break
    fi
    if [ "$attempt" -eq 20 ]; then
      echo "WARNING: service not reachable after 20s — check logs with: docker compose logs"
    fi
    sleep 1
  done

  echo "==> Done. Running at http://localhost:${port}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
