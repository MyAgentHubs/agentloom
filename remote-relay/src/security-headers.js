"use strict";
// security-headers.js — T6g1 · S4 同域静态托管的安全响应头。给 index.js 转发给 env.ASSETS.fetch()
// 的每一个静态资源响应（`remote-web` 的构建产物）叠加 CSP / Referrer-Policy / X-Content-Type-Options
// Cache-Control also prevents sensitive static responses from remaining in browser caches.
// Together, these headers constrain external resource access and referrer disclosure:
//
//   「静态响应带 Referrer-Policy: no-referrer + 严格 CSP（无第三方 script/analytics/远程字体·
//   connect-src 仅精确 relay 域）+ Cache-Control: no-store」
//
// **信任边界如实披露（同 remote-relay/README.md「这是什么、不是什么」一节）**：这些头防的是「第三方
// 脚本/字体/analytics 混进来」「fragment 泄漏进 Referer/被 3xx 继承」这类外部攻击面，不改变「同域
// 托管的 Web 端 E2EE 防外人、防入侵，不防运营方自己」这条已披露的取舍——CSP 挡不住 relay 运营方
// 自己下发一份读配对材料的恶意页面代码，那是拓扑形状本身的取舍，不是这份文件能修的。

// 内联 bootstrap 脚本（remote-web/index.html 里紧跟 <title> 的那段同步经典脚本，T6g1 新增——见该
// 文件头注）的 SHA-256 hash（base64 编码，CSP hash-source 语法 `'sha256-<这段值>'`）。CSP
// script-src 靠它精确放行这一段内联脚本；除了它，index.html 不含任何其它内联 <script>，也不允许
// `'unsafe-inline'` 兜底——没有入 hash 的内联脚本一律被浏览器挡掉。
//
// **联动防漂移（本单硬要求，见任务书 §2「构建流水」条）**：这个常量不是抄一次就一劳永逸——
// test/security-headers.test.js 会在测试期读 remote-web 的构建产物
// （`../remote-web/dist/index.html`），用与这里同样的算法现算那段内联脚本文字的 SHA-256，跟这个
// 常量断言相等。改了 index.html 那段内联脚本的文字内容（哪怕一个空格、或者 vite/构建工具链升级换了
// 输出格式）却忘了同步更新这个常量，这条测试会先红——不会等到线上浏览器悄悄把整个 App 的入口脚本
// 挡成白屏才被人发现。
export const BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64 = "TODD0fPdxdU/9xVQ8IkbnHo41ae1FO1vHcCwUsZQ4CQ=";

// dist/ 里带内容 hash 的静态资源目录前缀（vite 标准输出结构：`dist/assets/<name>-<hash>.js|css`）。
const HASHED_ASSET_PATH_PREFIX = "/assets/";

// A directory prefix alone cannot justify immutable caching: assets may retain stable names.
// Manually copied files and public assets can bypass content-hashed build output.
// If their contents change without a new filename, immutable caching would serve stale bytes.
// Require a filename suffix shaped like a build-generated content hash before caching forever.
// The expected suffix is a hyphen, a hash, a dot, and a file extension.
// Accept letters, digits, underscores, and hyphens used by the hash encoding.
// Leading hyphens and underscores in the hash must remain valid to match generated assets.
// Require at least eight hash characters to match the expected build naming convention.
// Filenames without this suffix cannot establish that a content change changes the URL.
// Those files retain the no-store default so stable URLs do not preserve stale content.
const HASHED_ASSET_FILENAME_RE = /-[A-Za-z0-9_-]{8,}\.\w+$/;

/**
 * 按请求 host 构造这次响应要用的 CSP 值——`connect-src` 需要精确到当前请求的 host（同源
 * `wss://<host>`，不用通配符/不留第三方口子），因为同一份 worker 代码要同时服务
 * `*.workers.dev` 开发域与未来的生产自定义域，CSP 不能写死某一个。
 */
export function buildContentSecurityPolicy(host) {
  return [
    "default-src 'self'",
    `script-src 'self' 'sha256-${BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64}'`,
    // 'unsafe-inline'（style-src 專用，风险量级远低于 script-src 的同名选项——内联 CSS 不能拿去
    // 执行 JS）：`remote-web` 经 `@app/components/MessageContent` 复用桌面共享的 markdown 渲染
    // 叶子，其依赖链（构建产物含 mermaid/katex，见 T6g1 report）在渲染图表/公式时会给它生成的
    // SVG/公式节点直接写内联 `style` 属性——不是 vite 往 `index.html` 注入了内联 `<style>`
    // 块（实测 `dist/index.html` 只有一个外链 `<link rel="stylesheet">`，见 T6g1 报告 ③ 表），是
    // 这条渲染管线运行期的行为，CSP 层面拦不掉又不想让整块 markdown 渲染在真机上静默丢样式，权衡
    // 后放开 style-src 这一项；`script-src` 不放这个口子（那才是能直接执行任意代码的风险）。
    "style-src 'self' 'unsafe-inline'",
    `connect-src 'self' wss://${host}`,
    "img-src 'self' data:",
    "base-uri 'none'",
    "frame-ancestors 'none'",
  ].join("; ");
}

/**
 * `pathname` 是否是可以放心长缓存 immutable 的那一类：**同时**落在 `/assets/` 目录下**且**文件名
 * 本身长得像 vite/rollup 产出的内容 hash（`HASHED_ASSET_FILENAME_RE`）。两个条件都要——目录前缀
 * 挡意外命中别的路径，文件名 hash 形态挡"/assets/ 目录下混进未带内容 hash 的文件"（见上方常量
 * 注释）；只有两条都满足，"文件名不变 = 内容不变"这个 immutable 缓存的前提才真的成立。
 */
export function isHashedAssetPath(pathname) {
  return pathname.startsWith(HASHED_ASSET_PATH_PREFIX) && HASHED_ASSET_FILENAME_RE.test(pathname);
}

/**
 * 给 `env.ASSETS.fetch()` 返回的响应叠加本文件定义的安全头，返回一个新 `Response`——不直接改
 * `assetResponse.headers`（部分运行时的 fetch `Response.headers` 是不可变的只读视图），`new
 * Response(body, response)` 是跨 Cloudflare Workers / Node undici 都成立的标准写法：把
 * `assetResponse` 当 `ResponseInit` 传入会复制它的 `status`/`statusText`/`headers`（复制出的
 * `headers` 是一份新的、可变的 `Headers` 实例），`body` 单独原样转发，不经过手动缓冲/重新读取。
 */
export function withSecurityHeaders(assetResponse, url) {
  const response = new Response(assetResponse.body, assetResponse);
  response.headers.set("Content-Security-Policy", buildContentSecurityPolicy(url.host));
  response.headers.set("Referrer-Policy", "no-referrer");
  response.headers.set("X-Content-Type-Options", "nosniff");
  response.headers.set(
    "Cache-Control",
    isHashedAssetPath(url.pathname) ? "public, max-age=31536000, immutable" : "no-store",
  );
  return response;
}
