#!/usr/bin/env bash
# Craft Anything 桌面应用：启动 / 停止 / 重启 / 状态
#
#   ./scripts/app.sh start      编译并后台启动（首次编译需要几分钟）
#   ./scripts/app.sh stop       停止（先请它正常退出，10 秒不退再强制结束）
#   ./scripts/app.sh restart    停止再启动
#   ./scripts/app.sh status     是否在运行
#
# 可选：PROFILE=debug ./scripts/app.sh start   用调试构建（编译快、运行慢），默认 release
# 进程号写在 target/dev-run/app.pid，应用日志在 target/dev-run/app.log
#
# 注意：中文标点紧跟在 $VAR 后面时，bash 3.2 会把标点并入变量名，所以变量一律写 ${VAR}。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${PROFILE:-release}"
BIN="${ROOT}/target/${PROFILE}/craft-anything"
RUN_DIR="${ROOT}/target/dev-run"
PID_FILE="${RUN_DIR}/app.pid"
LOG_FILE="${RUN_DIR}/app.log"

is_running() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }

# 找 cargo：先看 PATH，再回退 ~/.cargo/bin（rustup 装的，不是每个 shell 都带）
find_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    command -v cargo
  elif [ -x "${HOME}/.cargo/bin/cargo" ]; then
    printf '%s\n' "${HOME}/.cargo/bin/cargo"
  fi
}

# 打印仍在运行的 pid；pid 文件已过期则清掉并返回非零
current_pid() {
  [ -f "${PID_FILE}" ] || return 1
  local pid
  pid="$(cat "${PID_FILE}" 2>/dev/null || true)"
  if is_running "${pid}"; then
    printf '%s\n' "${pid}"
    return 0
  fi
  rm -f "${PID_FILE}"
  return 1
}

start() {
  local pid
  if pid="$(current_pid)"; then
    echo "已经在运行（pid ${pid}）。要重启：${0} restart"
    return 0
  fi
  local cargo_bin
  cargo_bin="$(find_cargo)"
  if [ -z "${cargo_bin}" ]; then
    echo "找不到 cargo：本项目需要 Rust 1.97+。先安装（https://rustup.rs），再重新运行本脚本。" >&2
    return 1
  fi
  # CJK 等字形在编译期从 craft-fonts 嵌入（见 docs/localization.md）：
  # 没设 CRAFT_FONTS_DIR 就自动用 ../craft-fonts；找不到就警告，否则界面中文会显示为方块。
  if [ -z "${CRAFT_FONTS_DIR:-}" ]; then
    if [ -f "${ROOT}/../craft-fonts/fonts/manifest.txt" ]; then
      CRAFT_FONTS_DIR="$(cd "${ROOT}/../craft-fonts" && pwd)"
      export CRAFT_FONTS_DIR
      echo "字体：${CRAFT_FONTS_DIR}"
    else
      echo "提示：未找到 craft-fonts（${ROOT}/../craft-fonts），构建不含 CJK 字体，界面中文会显示为方块。" >&2
    fi
  fi
  echo "编译中（profile=${PROFILE}，首次需要几分钟）…"
  if [ "${PROFILE}" = release ]; then
    "${cargo_bin}" build --release -p craft-anything
  else
    "${cargo_bin}" build -p craft-anything
  fi
  mkdir -p "${RUN_DIR}"
  # RUST_LOG=trace：让 winit/eframe 的重绘调度轨迹写进日志（app.log 与 desktop.log）。
  # 2026-11-07 界面曾整体停画 48 分钟而无任何报错；有 trace 才能定位这类"卡死"。
  export RUST_LOG="${RUST_LOG:-trace}"
  nohup "${BIN}" >"${LOG_FILE}" 2>&1 &
  pid=$!
  printf '%s\n' "${pid}" >"${PID_FILE}"
  sleep 1
  if is_running "${pid}"; then
    echo "已启动（pid ${pid}）"
    echo "日志：${LOG_FILE}"
  else
    rm -f "${PID_FILE}"
    echo "启动失败，最近日志："
    tail -n 20 "${LOG_FILE}" 2>/dev/null || true
    return 1
  fi
}

stop() {
  local pid
  if ! pid="$(current_pid)"; then
    echo "没有在运行。"
    return 0
  fi
  echo "正在停止（pid ${pid}）…"
  kill "${pid}" 2>/dev/null || true
  local i
  for i in $(seq 1 50); do
    is_running "${pid}" || break
    sleep 0.2
  done
  if is_running "${pid}"; then
    echo "10 秒内没有退出，强制结束。"
    kill -9 "${pid}" 2>/dev/null || true
    sleep 0.5
  fi
  rm -f "${PID_FILE}"
  if is_running "${pid}"; then
    echo "停止失败：进程 ${pid} 还在。" >&2
    return 1
  fi
  echo "已停止。"
}

status() {
  local pid
  if pid="$(current_pid)"; then
    echo "运行中（pid ${pid}）"
  else
    echo "未运行。"
  fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  restart) stop; start ;;
  status) status ;;
  *) echo "用法：${0} start|stop|restart|status" >&2; exit 2 ;;
esac
