#!/usr/bin/env bash
# 断言 boot-trace.log 里出现了「前端确实跑起来、页面确实挂载」的关键 label 序列。
# 用于 macOS 兼容冒烟 CI（.github/workflows/macos-compat-smoke.yml）：证明 app 在目标
# macOS 版本的系统 WebView 上没有卡白屏（JS 没跑起来 / 挂了 / 冻死在挂载前）。
#
# 用法：assert-boot-trace.sh <boot-trace.log 路径>
# 退出码：0 = 判据全部满足；非 0 = 缺失某个判据（stderr 说明缺哪条 + 打印完整日志辅助诊断）。
#
# The criteria are derived from the actual call sequence of traceBoot(...) in app/src/main.tsx:
#   1. "main.tsx module start"       —— 前端入口模块开始执行：JS 至少被 WebView 加载解析成功，
#                                        不是「资源 404 / 解析报错」那类最早期白屏。
#   2. "createRoot.render returned"  —— React createRoot().render(...) 这次同步调用正常返回
#                                        （没有在挂载阶段抛异常把整个模块炸掉、也没卡死在同步渲染里）。
#   3. "window shown"                —— Tauri 窗口从创建时的 visible:false 变成了可见：不是
#                                        「JS 跑起来了但窗口永远没被 show 出来」那种视觉上的白屏/黑屏。
#   4. "second rAF"                  —— 挂载之后 WebView 连续跑完两帧 requestAnimationFrame，
#                                        证明合成器没有冻死在空白帧上（旧 WebKit 版本的
#                                        PerformanceObserver({type:"paint"}) 支持不稳，FCP label
#                                        不保证一定出现，main.tsx 自己也把它当兜底而非硬依赖——所以
#                                        本脚本同样不把 FCP 当硬判据，只作为附加信息报告）。
#
# 只要以上 4 条都命中，就判定「启动到首屏成功、非白屏」。这 4 条本身互相独立、任一条缺失都足以
# 说明启动链路在该处卡住，因此用 AND（非 OR）语义逐条检查。
set -euo pipefail

LOG_PATH="${1:?用法：assert-boot-trace.sh <boot-trace.log 路径>}"

if [[ ! -f "${LOG_PATH}" ]]; then
  echo "FAIL：boot-trace.log 不存在：${LOG_PATH}（app 可能根本没启动到能调用 boot_trace 命令的阶段，" >&2
  echo "      或者本次是非 PROD 构建——main.tsx 的 traceBoot() 在非 PROD 下直接 no-op，不会落盘）" >&2
  exit 1
fi

missing=0
check_label() {
  local label="$1"
  # grep -F：把 label 当纯字符串子串匹配，不当正则——Rust 侧格式串按 28 字符右对齐补左侧空格，
  # 补白宽度会因 label 长度不同而变化，但 label 文本本身永远完整出现，子串匹配足够稳。
  if ! grep -qF -- "${label}" "${LOG_PATH}"; then
    echo "FAIL：缺少判据 label：${label}" >&2
    missing=1
  else
    echo "OK：命中 label：${label}"
  fi
}

check_label "main.tsx module start"
check_label "createRoot.render returned"
check_label "window shown"
check_label "second rAF"

if grep -qF -- "FCP" "${LOG_PATH}"; then
  echo "INFO：命中 FCP label（首次内容绘制的 PerformanceObserver 在这个 WebView 上生效）"
elif grep -qF -- "splash removed signal=timeout" "${LOG_PATH}"; then
  echo "WARN：splash 是靠 1000ms 兜底超时移除的，FCP 没有按预期触发——不算硬失败(second rAF 判据已经过)，" >&2
  echo "      但值得关注这个 macOS 版本上是不是渲染变慢了或 PerformanceObserver({type:paint}) 不受支持" >&2
fi

if [[ "${missing}" -ne 0 ]]; then
  echo "FAIL：boot-trace.log 缺失关键判据，判定本次启动可能白屏/卡死。完整日志如下：" >&2
  cat "${LOG_PATH}" >&2
  exit 1
fi

echo "PASS：boot-trace.log 关键 label 齐全，判定启动到首屏成功、非白屏。"
exit 0
