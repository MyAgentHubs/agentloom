"use strict";
// security-headers.test.js — T6g1 · S4 静态托管安全头逐项断言 + CSP hash 与 dist 实际内联脚本的
// 联动校验（防手改漂移：dist 变了、常量没跟着改，这条测试必须先红——见 src/security-headers.js
// 文件头注 + remote-web/index.html 内联脚本头注）。

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64,
  buildContentSecurityPolicy,
  isHashedAssetPath,
  withSecurityHeaders,
} from "../src/security-headers.js";

// ============================================================================
// CSP hash ↔ dist 实际产物联动（本单硬要求）
// ============================================================================

test("BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64 与 remote-web/dist/index.html 实际内联脚本的 SHA-256 一致", () => {
  let html;
  try {
    html = readFileSync(new URL("../../remote-web/dist/index.html", import.meta.url), "utf8");
  } catch (err) {
    assert.fail(
      `读不到 remote-web/dist/index.html（${err.message}）——本测试依赖已跑过 ` +
        "`cd remote-web && npm run build`；任务书 §3「收尾全绿才提交」的验收命令顺序就是先 " +
        "build 再跑两侧 npm test，不是这条测试自己的缺陷。",
    );
    return;
  }

  // 只匹配裸 `<script>`（无 type/src 等属性）——remote-web/index.html 里只有这一段是这种形状
  // （module 入口脚本带 `type="module" crossorigin src="..."`，不会误命中）；顺手断言"必须恰好
  // 一个"，防止未来往 index.html 加了第二个裸内联 <script> 却没人管它有没有入 CSP hash。
  const matches = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
  assert.equal(
    matches.length,
    1,
    `remote-web/dist/index.html 应当恰好含 1 个裸 <script>（fragment bootstrap）内联脚本，` +
      `实际找到 ${matches.length} 个——新增的裸内联脚本必须各自算 hash 并入 CSP script-src，` +
      "或者干脆别用内联脚本（改走带 src 的外部文件，天然不需要 hash）。",
  );

  const scriptText = matches[0][1];
  const actualHashBase64 = createHash("sha256").update(scriptText, "utf8").digest("base64");

  assert.equal(
    actualHashBase64,
    BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64,
    "remote-web/index.html 的 fragment bootstrap 内联脚本文字改动了（哪怕只改一个空格/一次构建" +
      "工具链升级换了输出格式），但 src/security-headers.js 的 BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64 " +
      "常量没有同步更新——按新脚本文字重新算一遍 SHA-256（base64 编码）并更新那个常量。这条测试就" +
      "是防止这种漂移在本地被漏掉、留到线上被浏览器 CSP 挡成白屏才发现。",
  );
});

test("buildContentSecurityPolicy 用的 script-src hash 与常量一致（常量本身接线正确）", () => {
  const csp = buildContentSecurityPolicy("relay.example.com");
  assert.match(csp, new RegExp(`sha256-${escapeRegExp(BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64)}`));
});

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// ============================================================================
// CSP 逐项断言（整条精确匹配——防止某一项悄悄漂移/被拼漏）
// ============================================================================

test("buildContentSecurityPolicy：workers.dev host 下的整条 CSP 精确匹配", () => {
  const csp = buildContentSecurityPolicy("relay.example.com");
  assert.equal(
    csp,
    "default-src 'self'; " +
      `script-src 'self' 'sha256-${BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64}'; ` +
      "style-src 'self' 'unsafe-inline'; " +
      "connect-src 'self' wss://relay.example.com; " +
      "img-src 'self' data:; " +
      "base-uri 'none'; " +
      "frame-ancestors 'none'",
  );
});

test("buildContentSecurityPolicy：connect-src 按当前请求 host 动态拼，不是写死值（未来自定义域同样生效）", () => {
  const csp = buildContentSecurityPolicy("relay.example.com");
  assert.match(csp, /connect-src 'self' wss:\/\/relay\.example\.com;/);
  assert.doesNotMatch(csp, /workers\.dev/);
});

test("CSP 不含第三方 script/font/analytics 来源（default-src 'self' 收紧到底，没有额外域名口子）", () => {
  const csp = buildContentSecurityPolicy("relay.example");
  // 除了 'self' 与本单唯一放行的 bootstrap hash，script-src 不该出现任何其它域名/协议 token。
  const scriptSrc = csp.split("; ").find((part) => part.startsWith("script-src"));
  assert.equal(scriptSrc, `script-src 'self' 'sha256-${BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64}'`);
});

// ============================================================================
// isHashedAssetPath / Cache-Control 分流
// ============================================================================

test("isHashedAssetPath：文件名长得像 vite 内容 hash 才命中，/assets/ 前缀本身不够", () => {
  // 正例：真实内容 hash 形态（来自实测 dist/assets 产物，见 src/security-headers.js 常量注释）。
  assert.equal(isHashedAssetPath("/assets/index-BTlQZ0UP.js"), true);
  assert.equal(isHashedAssetPath("/assets/index-B_5qTElV.css"), true);
  // hash 段本身以 "-" 开头的真实产物形态（chunk-2Q5K7J3B--8GybYSm.js）——base64url 字母表允许，
  // 不能被过窄的正则漏判。
  assert.equal(isHashedAssetPath("/assets/chunk-2Q5K7J3B--8GybYSm.js"), true);

  // 负例①②（本轮审查点名）：/assets/ 目录下但文件名没有内容 hash 后缀——immutable 的前提「文件名
  // 不变=内容不变」在这类文件上不成立，必须落 no-store 默认档，不能只看目录前缀就判 immutable。
  assert.equal(isHashedAssetPath("/assets/index.js"), false);
  assert.equal(isHashedAssetPath("/assets/logo.svg"), false);

  // 其它既有负例（非 /assets/ 路径，含 SPA fallback 命中的深链）。
  assert.equal(isHashedAssetPath("/"), false);
  assert.equal(isHashedAssetPath("/index.html"), false);
  assert.equal(isHashedAssetPath("/some/deep/spa/fallback/path"), false);
});

// ============================================================================
// withSecurityHeaders：四类头逐项断言 + 不破坏原响应的 body/status/其它 header
// ============================================================================

function fakeAssetResponse({ status = 200, contentType = "text/html; charset=utf-8", body = "<html></html>" } = {}) {
  return new Response(body, {
    status,
    headers: { "content-type": contentType, etag: '"fake-etag"' },
  });
}

test("withSecurityHeaders：index.html（非 /assets/ 路径）四头逐项正确，Cache-Control=no-store", async () => {
  const url = new URL("https://relay.example.com/");
  const response = withSecurityHeaders(fakeAssetResponse(), url);

  assert.equal(response.status, 200);
  assert.equal(
    response.headers.get("Content-Security-Policy"),
    buildContentSecurityPolicy("relay.example.com"),
  );
  assert.equal(response.headers.get("Referrer-Policy"), "no-referrer");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  // 原响应的其它头（content-type/etag）原样保留，没被覆盖/丢失。
  assert.equal(response.headers.get("content-type"), "text/html; charset=utf-8");
  assert.equal(response.headers.get("etag"), '"fake-etag"');
  assert.equal(await response.text(), "<html></html>");
});

test("withSecurityHeaders：/assets/ 下的带 hash 静态文件长缓存 immutable", () => {
  const url = new URL("https://relay.example.com/assets/index-BTlQZ0UP.js");
  const response = withSecurityHeaders(
    fakeAssetResponse({ contentType: "text/javascript; charset=utf-8" }),
    url,
  );
  assert.equal(response.headers.get("Cache-Control"), "public, max-age=31536000, immutable");
  // 安全头仍然统一叠加（任务书原话「统一加安全头」，不分文件类型）。
  assert.equal(response.headers.get("Referrer-Policy"), "no-referrer");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  assert.ok(response.headers.get("Content-Security-Policy").includes("default-src 'self'"));
});

test("withSecurityHeaders：/assets/index.js（无内容 hash 后缀）no-store，不因目录前缀误判 immutable", () => {
  const url = new URL("https://relay.example.com/assets/index.js");
  const response = withSecurityHeaders(
    fakeAssetResponse({ contentType: "text/javascript; charset=utf-8" }),
    url,
  );
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("withSecurityHeaders：/assets/logo.svg（无内容 hash 后缀）no-store，不因目录前缀误判 immutable", () => {
  const url = new URL("https://relay.example.com/assets/logo.svg");
  const response = withSecurityHeaders(fakeAssetResponse({ contentType: "image/svg+xml" }), url);
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("withSecurityHeaders：SPA fallback 命中的深链路径（非 /assets/）同样 no-store，不当成可长缓存资源", () => {
  const url = new URL("https://relay.example.com/some/deep/link");
  const response = withSecurityHeaders(fakeAssetResponse(), url);
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("withSecurityHeaders：不改写原响应 status（根路径禁 3xx 的前提——本函数本身绝不引入重定向）", () => {
  const url = new URL("https://relay.example.com/");
  const response = withSecurityHeaders(fakeAssetResponse({ status: 200 }), url);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("location"), null);
});
