#!/usr/bin/env bash
# check-webview-compat.sh — 产物 WebView 兼容扫描（Safari 16 基线出包门禁）
#
# 背景：真实用户在 macOS 13.0（WKWebView = Safari 16.0/16.1）遇到过 dmg
# 白屏。事后 RCA 结论是「入口产物当时其实是干净的、白屏另有根因（零错误
# 兜底，已修）」——但「产物不含 Safari 16 不支持的语法/运行时 API」这条
# 兼容底线，目前只靠 vite.config.ts 的 build.target: "safari16" 隐式撑
# 着：没有任何机械检查能防住「有人写了 Safari 16 不支持的语法/API、构建
# 工具翻译不了（运行时特性而非语法）、或未来 target 被悄悄改动」这类回
# 归。本脚本把当时手工排查用过的扫描手法自动化，接进出包门禁（见
# app/scripts/release-macos.sh 里调用处）。
#
# ============================================================================
# 分级设计（本脚本的核心决策，改动前先读懂这段，别改错了语义）
# ============================================================================
# 「入口 chunk」＝ dist/index.html 里 <script type="module" src=...>（以及
# <link rel="modulepreload" href=...>，虽然当前构建没用到，防将来加回）
# 直接引用、并沿静态 import（`import ... from "./x.js"` / 裸
# `import "./x.js"`）链传递可达的全部文件。浏览器渲染出首屏之前必须同步
# 解析执行这些文件——其中任何一个解析失败，都是「整页起不来」的白屏。
#   => 入口 chunk 违规 = 阻断发布（exit 1）。
#
# 其余 dist/assets/*.js 只通过运行时 import() 动态加载（这个项目里典型
# 例子是 mermaid / katex / 各 diagram 类型 / 各语言语法高亮包这类重量级、
# 按需拉起的代码，经实勘当前入口 chunk 对它们全部是动态 import()，零静态
# 互引）。它们的加载点挂在具体功能路径上——用户点开某个 diagram、某种代
# 码块语法高亮才会触发——解析失败只炸那一个功能，不影响启动、不是白屏。
#   => 懒加载 chunk 违规 = 只警告、不阻断，但打印完整清单方便追踪修。
#
# CSS 特性只记录、不阻断：这类通常是渐进增强（圆角/视觉效果类特性），
# 浏览器不认得时是优雅降级，不是白屏。
# ============================================================================
#
# 用法：
#   check-webview-compat.sh [产物目录]     # 默认 app/dist；扫描并按上面
#                                           # 的分级规则决定 exit 0/1
#   check-webview-compat.sh --self-test    # 自检模式：拿脚本内置的 fixture
#                                           # （不落盘进仓库）验证「扫描器
#                                           # 真的会响」——对应
#                                           # docs 里「静默 fail-open 最危
#                                           # 险」的教训，扫描器上线前必须
#                                           # 自证会报警，不能默认信任
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
ESBUILD_BIN="${APP_DIR}/node_modules/.bin/esbuild"

# ---------------------------------------------------------------------------
# 常量区：调整兼容基线只改这里就够（比如未来要支持 macOS 12，把
# ESBUILD_TARGET 改成 safari15，并同步改 vite.config.ts / tauri.conf.json
# 的 minimumSystemVersion——三处基线必须一致，本脚本不替你同步那两处）。
# ---------------------------------------------------------------------------
ESBUILD_TARGET="safari16"

# JS 特性清单：并行数组（下标一一对应）。bash 3.2（macOS 系统自带版本）
# 没有关联数组，用两个下标对齐的数组代替 map。
FEATURE_NAMES=(
  "原生正则 lookbehind 字面量 /(?<=...)/ 或 /(?<!...)/"
  "class static 初始化块 static { ... }"
  "Object.groupBy(...)"
  "Array.prototype.toSorted(...)"
  "Array.prototype.toReversed(...)"
  "Array.prototype.toSpliced(...)"
  "structuredClone(...)"
  "import.meta.resolve(...)"
)
FEATURE_PATTERNS=(
  '/\(\?<[=!]'
  'static[[:space:]]*\{'
  'Object\.groupBy\('
  '\.toSorted\('
  '\.toReversed\('
  '\.toSpliced\('
  'structuredClone\('
  'import\.meta\.resolve'
)
# 已知但本刀不纳入扫描：Array.prototype.with(...) / TypedArray#with(...)。
# `.with(` 在真实代码里到处是同名方法/链式调用（选项对象 builder、Promise
# 链……），纯文本 grep 假命中率极高，纳入只会淹没真实信号、把人训练成无视
# 报告。等有更准的判别手段（比如结合 AST 而不是纯文本）再启用；先记录在
# 案，别当作「漏检」。

# CSS 特性清单：只记录、不阻断（见上方分级设计）。
CSS_FEATURE_NAMES=(
  "color-mix()（Safari 16.2+ 才支持，16.0/16.1 不认）"
  "content-visibility（Safari 18+ 才支持）"
  ":has() 关系选择器（Safari 16.4+ 才支持）"
  "@container 容器查询（子特性支持程度不一，需逐个确认）"
  "@layer 层叠层（Safari 16.4+ 才支持）"
)
CSS_FEATURE_PATTERNS=(
  'color-mix\('
  'content-visibility'
  ':has\('
  '@container'
  '@layer'
)

print_usage() {
  cat <<'EOF'
用法：check-webview-compat.sh [产物目录]
      check-webview-compat.sh --self-test

产物目录默认 app/dist。--self-test 用脚本内置的 fixture 自检扫描器本身
会不会响（不读写仓库或产物目录，纯临时目录）。
EOF
}

# ---------------------------------------------------------------------------
# 可变状态（每次 scan_dist_dir 调用前必须 reset_scan_state，供 --self-test
# 连续跑多个 fixture 时互不串味）
# ---------------------------------------------------------------------------
TOTAL_JS=0
TOTAL_CSS=0
ENTRY_COUNT=0
LAZY_COUNT=0
BLOCKING_REPORT=""
WARN_REPORT=""
CSS_REPORT=""
HAS_BLOCKING=0

reset_scan_state() {
  TOTAL_JS=0
  TOTAL_CSS=0
  ENTRY_COUNT=0
  LAZY_COUNT=0
  BLOCKING_REPORT=""
  WARN_REPORT=""
  CSS_REPORT=""
  HAS_BLOCKING=0
}

# 检查单个 JS 文件。$1=文件绝对路径 $2=报告里显示用的相对名 $3=severity
# （entry|lazy）。命中特性清单或 esbuild 报警都算「违规」；entry 记入阻断
# 清单，lazy 记入警告清单——两条清单互斥、由调用方传入的 severity 决定。
check_js_file() {
  local file="$1" label="$2" severity="$3"
  local findings="" i name pattern hits
  local esbuild_stderr esbuild_status

  for i in "${!FEATURE_NAMES[@]}"; do
    name="${FEATURE_NAMES[$i]}"
    pattern="${FEATURE_PATTERNS[$i]}"
    set +e
    hits="$(grep -oE -- "${pattern}" "${file}" 2>/dev/null | wc -l | tr -d ' ')"
    set -e
    if [ -n "${hits}" ] && [ "${hits}" != "0" ]; then
      findings="${findings}    - 特性清单命中：${name}（${hits} 处）"$'\n'
    fi
  done

  set +e
  esbuild_stderr="$("${ESBUILD_BIN}" --target="${ESBUILD_TARGET}" --log-level=warning "${file}" 2>&1 >/dev/null)"
  esbuild_status=$?
  set -e
  if [ "${esbuild_status}" -ne 0 ] || [ -n "${esbuild_stderr}" ]; then
    findings="${findings}    - esbuild --target=${ESBUILD_TARGET} 校验未通过（exit=${esbuild_status}）："$'\n'
    findings="${findings}$(printf '%s\n' "${esbuild_stderr}" | sed 's/^/      /')"$'\n'
  fi

  if [ -n "${findings}" ]; then
    if [ "${severity}" = "entry" ]; then
      HAS_BLOCKING=1
      BLOCKING_REPORT="${BLOCKING_REPORT}  [入口 chunk·阻断] ${label}"$'\n'"${findings}"
    else
      WARN_REPORT="${WARN_REPORT}  [懒加载 chunk·警告] ${label}"$'\n'"${findings}"
    fi
  fi
}

# 检查单个 CSS 文件：只记录，不影响 HAS_BLOCKING。
check_css_file() {
  local file="$1" label="$2"
  local findings="" i name pattern hits

  for i in "${!CSS_FEATURE_NAMES[@]}"; do
    name="${CSS_FEATURE_NAMES[$i]}"
    pattern="${CSS_FEATURE_PATTERNS[$i]}"
    set +e
    hits="$(grep -oE -- "${pattern}" "${file}" 2>/dev/null | wc -l | tr -d ' ')"
    set -e
    if [ -n "${hits}" ] && [ "${hits}" != "0" ]; then
      findings="${findings}    - 记录（不阻断）：${name}（${hits} 处）"$'\n'
    fi
  done

  if [ -n "${findings}" ]; then
    CSS_REPORT="${CSS_REPORT}  [CSS·仅记录] ${label}"$'\n'"${findings}"
  fi
}

# 从 index.html 里解析同步入口引用：<script type="module" src=...> 与
# <link rel="modulepreload" href=...>（当前构建没有后者，防将来加回）。
# 用 python3 而不是纯 grep/sed，是因为要不依赖属性书写顺序稳健提取
# src/href 属性值——这与 release-macos.sh 里解析 tauri.conf.json 版本号
# 用 python3 一次性脚本是同一惯例。
extract_entry_seeds() {
  local index_html="$1"
  python3 - "${index_html}" <<'PY'
import re
import sys

html = open(sys.argv[1], encoding="utf-8").read()
seeds = []
for tag in re.findall(r"<script\b[^>]*>", html):
    if 'type="module"' not in tag:
        continue
    m = re.search(r'src="([^"]+)"', tag)
    if m:
        seeds.append(m.group(1))
for tag in re.findall(r"<link\b[^>]*>", html):
    if 'rel="modulepreload"' not in tag:
        continue
    m = re.search(r'href="([^"]+)"', tag)
    if m:
        seeds.append(m.group(1))
print("\n".join(seeds))
PY
}

# 扫描一个产物目录，打印人话报告，通过 return 值传回 0（通过）/1（阻断）。
# 每次调用前必须 reset_scan_state；--self-test 会对多个 fixture 目录连续
# 调用本函数。
scan_dist_dir() {
  local dist_dir="$1"

  if [ ! -d "${dist_dir}" ]; then
    echo "错误：产物目录不存在：${dist_dir}" >&2
    return 1
  fi
  dist_dir="$(cd "${dist_dir}" && pwd)"

  local index_html="${dist_dir}/index.html"
  if [ ! -f "${index_html}" ]; then
    echo "错误：产物目录缺少 index.html：${index_html}" >&2
    return 1
  fi

  local seeds
  seeds="$(extract_entry_seeds "${index_html}")"
  if [ -z "${seeds}" ]; then
    echo "错误：index.html 里没解析到任何 <script type=\"module\"> 入口，无法定位入口 chunk（拒绝静默放行）：${index_html}" >&2
    return 1
  fi

  # entry files：先用 index.html 里的种子引用，再沿静态 import 做闭包。
  local entry_files=()
  local rel
  while IFS= read -r rel; do
    [ -z "${rel}" ] && continue
    rel="${rel%%\?*}"
    rel="${rel#./}"
    rel="${rel#/}"
    entry_files+=("${dist_dir}/${rel}")
  done <<< "${seeds}"

  local idx=0 cur cur_dir refs ref cand already existing
  while [ "${idx}" -lt "${#entry_files[@]}" ]; do
    cur="${entry_files[${idx}]}"
    idx=$((idx + 1))
    [ -f "${cur}" ] || continue
    cur_dir="$(dirname "${cur}")"
    set +e
    refs="$(grep -oE '(from|import)"\./[A-Za-z0-9_.-]+\.(js|mjs)"' "${cur}" 2>/dev/null | grep -oE '\./[A-Za-z0-9_.-]+\.(js|mjs)')"
    set -e
    while IFS= read -r ref; do
      [ -z "${ref}" ] && continue
      cand="${cur_dir}/${ref#./}"
      already=0
      for existing in "${entry_files[@]}"; do
        if [ "${existing}" = "${cand}" ]; then
          already=1
          break
        fi
      done
      if [ "${already}" -eq 0 ]; then
        entry_files+=("${cand}")
      fi
    done <<< "${refs}"
  done

  echo "== WebView 兼容扫描：${dist_dir} =="
  echo "兼容基线：${ESBUILD_TARGET}（对应真实用户 macOS 13.0 的 WKWebView）"
  echo ""
  echo "入口 chunk（${#entry_files[@]} 个，渲染首屏前必须同步解析——违规阻断发布）："
  local f
  for f in "${entry_files[@]}"; do
    if [ -f "${f}" ]; then
      echo "  - ${f#${dist_dir}/}"
    else
      echo "  - ${f#${dist_dir}/}（未找到，跳过——大概率是入口引用了目录外资源，非本次关注范围）"
    fi
  done
  echo ""

  local file rel_name
  for f in "${entry_files[@]}"; do
    [ -f "${f}" ] || continue
    TOTAL_JS=$((TOTAL_JS + 1))
    ENTRY_COUNT=$((ENTRY_COUNT + 1))
    check_js_file "${f}" "${f#${dist_dir}/}" "entry"
  done

  if [ -d "${dist_dir}/assets" ]; then
    for file in "${dist_dir}"/assets/*.js; do
      [ -e "${file}" ] || continue
      already=0
      for existing in "${entry_files[@]}"; do
        if [ "${existing}" = "${file}" ]; then
          already=1
          break
        fi
      done
      if [ "${already}" -eq 1 ]; then
        continue
      fi
      TOTAL_JS=$((TOTAL_JS + 1))
      LAZY_COUNT=$((LAZY_COUNT + 1))
      rel_name="${file#${dist_dir}/}"
      check_js_file "${file}" "${rel_name}" "lazy"
    done

    for file in "${dist_dir}"/assets/*.css; do
      [ -e "${file}" ] || continue
      TOTAL_CSS=$((TOTAL_CSS + 1))
      rel_name="${file#${dist_dir}/}"
      check_css_file "${file}" "${rel_name}"
    done
  fi

  echo "检查范围：JS ${TOTAL_JS} 个（入口 ${ENTRY_COUNT} / 懒加载 ${LAZY_COUNT}），CSS ${TOTAL_CSS} 个。"
  echo ""

  if [ -n "${BLOCKING_REPORT}" ]; then
    echo "---- 入口 chunk 违规（阻断） ----"
    printf '%s' "${BLOCKING_REPORT}"
  else
    echo "入口 chunk：未发现 ${ESBUILD_TARGET} 基线之外的特性/语法。"
  fi
  echo ""

  if [ -n "${WARN_REPORT}" ]; then
    echo "---- 懒加载 chunk 违规（仅警告，不阻断，建议追踪修） ----"
    printf '%s' "${WARN_REPORT}"
  else
    echo "懒加载 chunk：未发现 ${ESBUILD_TARGET} 基线之外的特性/语法。"
  fi
  echo ""

  if [ -n "${CSS_REPORT}" ]; then
    echo "---- CSS 特性清单（仅记录，渐进增强不阻断） ----"
    printf '%s' "${CSS_REPORT}"
  else
    echo "CSS：未发现清单里的渐进增强特性。"
  fi
  echo ""

  if [ "${HAS_BLOCKING}" -eq 1 ]; then
    echo "结论：产物含超出 Safari 16 基线的特性（入口 chunk 命中，见上方阻断清单）——门禁不通过。"
    return 1
  fi
  echo "结论：入口 chunk 兼容 Safari 16 基线，门禁通过。"
  return 0
}

# ---------------------------------------------------------------------------
# --self-test：证明「扫描器真的会响」，而不是默认信任它。对应「静默
# fail-open 最危险」的教训——上线前必须亲眼看到它对已知违规报警、对干净
# 产物放行，两头都要见到，不能只看一头。
# ---------------------------------------------------------------------------
run_self_test() {
  # 不用 local：这个变量要被下面的 EXIT trap 引用，而 trap 是在整个脚本
  # 退出时才触发（可能晚于 run_self_test 这个函数已经 return 之后），若
  # 声明成函数局部变量，trap 触发时变量已经出了作用域，在 set -u 下会
  # 报 "unbound variable"。用脚本级全局变量，生命周期覆盖到真正 exit。
  SELF_TEST_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/webview-compat-selftest.XXXXXX")"
  trap 'rm -rf "${SELF_TEST_TMP_DIR}"' EXIT

  make_fixture() {
    local name="$1" entry_body="$2"
    mkdir -p "${SELF_TEST_TMP_DIR}/${name}/assets"
    cat > "${SELF_TEST_TMP_DIR}/${name}/index.html" <<EOF
<!doctype html>
<html><head><script type="module" src="/assets/entry.js"></script></head>
<body></body></html>
EOF
    printf '%s\n' "${entry_body}" > "${SELF_TEST_TMP_DIR}/${name}/assets/entry.js"
  }

  make_fixture "clean" 'console.log("clean fixture: no Safari-16-incompatible feature here");'
  make_fixture "lookbehind" 'const re = /(?<=foo)bar/; console.log(re.test("foobar"));'
  make_fixture "staticblock" 'class Foo { static #x; static { Foo.#x = 1; } } console.log(Foo);'

  local overall_ok=1

  run_case() {
    local name="$1" expected="$2"
    echo ""
    echo "############################################################"
    echo "自检 fixture：${name}（预期 exit=${expected}）"
    echo "############################################################"
    reset_scan_state
    local actual=0
    scan_dist_dir "${SELF_TEST_TMP_DIR}/${name}" || actual=$?
    echo ""
    if [ "${actual}" -eq "${expected}" ]; then
      echo "自检结果：PASS（fixture=${name}，实际 exit=${actual}，符合预期）"
    else
      echo "自检结果：FAIL（fixture=${name}，实际 exit=${actual}，预期 exit=${expected}）——扫描器没有按预期响应，禁止信任本脚本，先修脚本再接门禁。"
      overall_ok=0
    fi
  }

  run_case "clean" 0
  run_case "lookbehind" 1
  run_case "staticblock" 1

  echo ""
  echo "============================================================"
  if [ "${overall_ok}" -eq 1 ]; then
    echo "自检总结：PASS —— 扫描器对干净产物放行、对已知违规 fixture 报警且阻断，行为符合预期。"
    return 0
  fi
  echo "自检总结：FAIL —— 见上方各 fixture 明细。"
  return 1
}

# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------
if [ ! -x "${ESBUILD_BIN}" ] && [ ! -f "${ESBUILD_BIN}" ]; then
  echo "错误：找不到 esbuild（${ESBUILD_BIN}），请先 npm --prefix app ci。" >&2
  exit 1
fi

case "${1:-}" in
  --self-test)
    run_self_test
    exit $?
    ;;
  -h|--help)
    print_usage
    exit 0
    ;;
esac

TARGET_DIST_DIR="${1:-${APP_DIR}/dist}"
reset_scan_state
scan_dist_dir "${TARGET_DIST_DIR}"
exit $?
