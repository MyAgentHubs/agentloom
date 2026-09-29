# AgentLoom Remote Control relay 骨架

[English](README.md) · **简体中文**

Cloudflare Workers + Durable Objects 版 relay 骨架，覆盖
**S1 接入与鉴权 / S2 房间 DO / S3 配额与滥用防护** 三块（单层信封、`kind`
含 `live`、`command_id` 为信封顶层字段——本轮修复把上一版骨架自己猜的
外层 `{envelope, milestone, command_id}` 包装拆掉了，详见下方「协议形状」
一节）。

## 这是什么、不是什么

relay 是一台「传话服务器」：桌面 AgentLoom 和远端（手机/浏览器）都主动出站
连它，它在两边之间搬**密文信封** + 一点路由用的元数据。

**relay 只见密文**：`src/envelope.js` 只解析/校验信封的外层字段（v / room /
epoch / seq / kind / session / ct / n / ts），从不尝试解密 `ct`——relay 没有
任何内容钥匙（`K_room` 只在桌面和已配对的远端设备手里）。这是
E2EE 的边界，也是整个方案「relay 挂了/被黑了也读不到会话内容」这条承诺的
代码落点。

**同域托管这条边界，如实转述**：Web 远端（另一个组件 C1/S4，
本骨架未包含）如果和 relay 同域托管，解密用的 JS 代码就是 relay 运营方下发
的——理论上运营方随时能推一份偷钥匙的版本。也就是说**同域托管的 Web 端
E2EE 防外人、防入侵，不防运营方自己**。这不是本骨架的实现缺陷，是这个拓扑
形状本身的取舍，等未来的 iOS 原生端才能给出更完整的承诺。

## 协议形状（单层信封，无外层包装）

WS 消息要么是一个**明文控制帧**（顶层 `t` 字段，presence /
`control.notify_hint` / `input.ack` 三种，`ct`/`n` 都没有），要么本身就是
一个**信封**——不再有 `{ envelope: ... }` 外层包装：

```
{ v, room, epoch,
  kind,        // "event"(里程碑·relay 盖 seq+落库) | "live"(只转发·永不落库·seq 恒 null)
               // | "input" | "control" | "presence"
  session,     // sid 或 null
  command_id,  // 可选·仅 kind=input（以及明文帧 input.ack）会带·relay 可读·入 AAD
  seq,         // 仅 kind=event·relay 盖·不入 AAD
  ct, n, ts }

AAD = v | room | epoch | kind | session | command_id
```

上一版骨架自己猜了一层 `{ envelope, milestone, command_id }` 外层包装、且
把 `milestone`/`command_id` 当成协议里没写、relay 自己需要的路由手柄。
现已按复核结果订正——`kind` 本身就是路由手柄（`event`/`live` 两个
值分别对应旧版「event + milestone 布尔位」的两种组合），`command_id` 正式
转正为信封顶层字段。详细理由见 `src/room-do.js` / `src/envelope.js` 文件头。

## 这个骨架做了什么

- **S1 接入与鉴权**（`src/auth.js` + `src/room-do.js` 的 `fetch`）：两种
  凭据、两种身份。桌面端用 `Authorization: Bearer` 头带房间所有者凭据，
  relay 对它做哈希后与认领房间时登记的哈希做常量时间比对，通过则以
  `desktop` 身份接入；远端（手机/浏览器）通过 WebSocket 子协议带凭据，
  子协议里必须恰好有一个版本项 `agentloom-rc-v1` 和一个 `token.<64 位
  十六进制>` 项，relay 再到该房间的令牌登记表里查这个令牌对应的设备，
  通过则以 `remote` 身份接入。查询串里不再接受令牌。**默认拒绝**：没带
  凭据、子协议格式有歧义、房间没登记过对应令牌、令牌过期或已被吊销，一律
  401，鉴权失败过于频繁则回 429；不存在匿名能连上看看的路径。令牌的
  **签发与登记**由桌面端在配对之后通过控制帧写进 relay，relay 只负责比对与
  到期处理。**鉴权先于落库**：业务表要等鉴权通过之后才会创建，鉴权失败的
  请求不会给房间落下任何行。
- **S2 房间 DO**（`src/room-do.js` + `src/room-store.js`）：一个 DO = 一个
  房间，用 **Hibernation API**（`ctx.acceptWebSocket` /
  `webSocketMessage` / `webSocketClose`，不是 `ws.accept()`）接 WebSocket，
  DO 内 SQLite 存里程碑事件日志（`events` 表，`seq` 由 `room_meta` 里一个
  独立单调计数器分配，不是 `MAX(events.seq)+1`——防未来加保留窗裁剪后 seq
  往回掉）+ 每设备连接的少量元数据（`ws.serializeAttachment()`，
  实现里有 ≤16KB 的防线）。
- **epoch 防双写**：桌面每次连上来，DO 把房间 epoch +1；`insertMilestone`
  拒绝 `epoch` 落后于房间当前 epoch 的写入（旧连接没死透又来了一个新连接
  时，旧连接的写入会被挡）。
- **kind 本身决定落不落库**：`kind=event` 恒是里程碑，落库、盖单调递增
  的 `seq`；`kind=live` 恒只转发、不落库、`seq` 恒 `null`；presence 不落库。
- **两条通道**：`kind=input` 走 FIFO——桌面在线直转，桌面离线则暂存进
  `pending_input` 表（30 分钟 TTL，过期广播 `input.expired` 并丢弃）；
  `kind=control` 即刻投递——桌面在线才转发，桌面离线直接告知发送方
  `desktop_offline`（不暂存，因为「插队通道」的语义就是现在生效或不生效，
  不该有「攒到明天生效」这种事）。
- **重连补发**：远端带 `?last_seq=N` 连入，DO 先回一帧
  `{t:"replay.head", epoch, headSeq}`，再把 `seq > N` 的里程碑按 seq 升序
  逐条推送，之后才接 live。
- **S3 配额，断 live、保里程碑**：按房间
  「里程碑写入条数/月」计数（`quota_counters` 表，UTC 年-月分桶）；超过
  `MONTHLY_MILESTONE_LIMIT`（默认值见 `src/quota.js`，生产数值待产品侧
  另定）后，只有 `kind=live` 被降级丢弃、并把
  `{t:"quota.exceeded", channel:"live"}` 广播给房间内所有连接（含远端，
  不是只回桌面）；`kind=event`（里程碑）**永不因配额降级**——丢里程碑等于
  产品承诺的历史记录出现补不回来的空洞，比多算一点成本严重得多；
  `kind=control` 永远放行（配额闸不该连 Stop 都按不动）；匿名/无令牌走不到
  这一步，S1 那关就先拒了。
- **role 强制方向**（`src/room-do.js` 的 `webSocketMessage`/
  `handlePlainFrame`）：连接的 role 由接入时验过的凭据决定（所有者凭据 =
  `desktop`，子协议令牌 = `remote`），不由客户端自报。relay 据此把读得到的
  方向管住：拒绝
  `role=remote` 发 `kind=event`（否则任何持有 `K_room` 的远端都能伪造一条
  「agent 说的话」广播、还会被落库/被将来的重连当成真实历史回放）；拒绝
  `role=desktop` 发 `kind=input`；`input.ack` 与 `control.notify_hint`
  仅接受来自桌面连接。违反方向的一律拒绝 + 回 `{t:"error",
  reason:"role_forbidden"}`，不静默丢弃。

## S4 同域静态托管 + 安全头

`remote-web`（C1 手机 Web 端）构建产物挂在这个 worker 同域下，`wrangler.toml`
的 `[assets]` 绑定指向 `../remote-web/dist`——**部署/`wrangler dev` 联调静态
资源之前必须先手动 build 一次**：

```bash
cd remote-web
npm run build   # 生成/刷新 remote-web/dist/——这一步不会被 wrangler 自动触发
```

`src/index.js` 的 `fetch()` 路由分流（`run_worker_first = true`，所有请求都先
进这里，见 `wrangler.toml` 注释）：`/healthz`、`/room/*`（WS 升级/claim 限速）
继续走原有路由、行为不变；其余 GET/HEAD 请求转发 `env.ASSETS.fetch()`
（`not_found_handling = "single-page-application"`——手机端是纯 URL fragment
路由的 SPA，任何未命中真实文件的路径都拿 index.html 应答，200 不是 3xx），响应
经 `src/security-headers.js::withSecurityHeaders()` 统一叠加四类安全头：
`Content-Security-Policy`（script-src 只放行 `'self'` + `remote-web/index.html`
里那段 fragment bootstrap 内联脚本的 SHA-256 hash，不留 `'unsafe-inline'` 口子）
/ `Referrer-Policy: no-referrer` / `X-Content-Type-Options: nosniff` /
`Cache-Control`（`/assets/` 下带内容 hash 的文件长缓存 immutable，其余
`no-store`）。

**CSP hash 与 dist 实际内联脚本的联动（防手改漂移）**：
`src/security-headers.js` 的 `BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64` 常量不是
抄一次就完事——`test/security-headers.test.js` 会在测试期读
`remote-web/dist/index.html` 现算那段内联脚本文字的 SHA-256，跟这个常量断言
相等；`remote-web/index.html` 的 fragment bootstrap 脚本改了文字却忘了同步
更新常量，这条测试先红，不会等到线上 CSP 把整个 App 挡成白屏才被发现。

**fragment 安全边界如实披露（同「这是什么、不是什么」一节的 E2EE 边界口径）**：
这批安全头防的是「第三方脚本/字体混进来」「配对 fragment 被 3xx 继承/落进
Referer」这类外部攻击面，不改变「同域托管的 Web 端 E2EE 防外人、防入侵，不防
relay 运营方自己」这条已披露的取舍——CSP 挡不住运营方自己下发一份读配对材料
的恶意页面代码，那是拓扑形状本身的取舍。

## 未做（本骨架的边界，不是遗漏）

- 生产配额数值（`DEFAULT_MONTHLY_MILESTONE_LIMIT` 是骨架能跑通用的占位值，
  不是产品拍板的数字）。
- staging 部署 + 手机真机矩阵冒烟 + 首屏预算数值门禁单独跟进；本节
  只覆盖同域托管的代码/配置/安全头，不跑 `wrangler deploy`。
- 桌面侧令牌签发 / 配对握手的完整流程在桌面端实现，本目录的 relay 只负责
  登记令牌、比对令牌和执行到期。
- **速率限流的范围**（跟 S3 的「月度里程碑配额」是两码事，见
  `src/quota.js` 文件头——配额管「这个月写太多行」，限流管「这一秒钟讲话
  太快」）：`wrangler.toml` 的 `RL_CLAIM`/`RL_UPGRADE` 在边缘按来源 IP 覆盖
  claim 与 WS upgrade 两个入口；房间内另有按来源 IP 计的鉴权失败限流、
  远端入站帧限流、配对握手帧限流、按设备限制并发连接数，以及 input/control
  帧的按设备窗口限流（实现见 `src/room-do-ratelimit.js`）。这些限流的内存
  计数在 DO 休眠后会重置；没有按房间整体计的消息限流。
- **L2**：seq 分配（`room-store.js` 的 `allocateSeq`）从「读计数器」到
  「写回 +1」之间不能有 `await`——目前全同步天然满足，注释已点明这个
  不变量，但没有专门的并发测试去钉死它。
- **L3**：`onlineDesktop()` 目前直接取 `getWebSockets("desktop")` 的第一个，
  没有处理「新旧两个桌面连接短暂并存」时该挑 epoch 更大的那个。
- **L4**：`pending_input` 的 FIFO 顺序目前只按 `created_at` 排序，没有
  rowid 之类的次级排序去打破同毫秒并发入队的顺序不确定性。
- **L5**：`validateEnvelope` 没有强制 `kind=live`/`presence` 的信封
  `seq` 必须是 `null`——目前只校验 `seq` 是合法整数或 null，没有按 kind
  收紧。

## 本地跑法

```bash
cd remote-relay
npm install
npm test               # 纯逻辑单测（envelope / auth / quota / room-store / room-do / S4 静态托管+安全头）

# 联调静态资源（S4）/ 本地起 DO 环境前，先 build 一次 remote-web（wrangler 不会自动触发这一步）：
(cd ../remote-web && npm run build)

npx wrangler dev        # 起本地 DO 环境，联调用；需要 wrangler 能访问网络
npx wrangler deploy --dry-run   # 只编译校验，不会真的发布
```

## 测试怎么跑起来的

`src/room-store.js` 的 SQL 逻辑不依赖 Cloudflare 专属 API，只依赖一个最小
适配器接口 `sql.exec(query, ...params) -> Array<行对象>`。`room-do.js` 里
用一个薄适配器包 `ctx.storage.sql`（Cloudflare DO 的真实 SQLite）；
`test/room-store.test.js` 用一个薄适配器包 Node 20+ 内建的 `node:sqlite`
（真实 SQLite，不是手搓的假实现）。两边跑的是**同一份** `room-store.js`
代码，不是「测试另写一份看起来像的逻辑」——seq 分配、epoch 拒绝、回放
查询、配额计数这几块因此是真被验证过的，不是自证。

`test/room-do.test.js` 给 `room-do.js` 搭了一个 mock-ctx
替身：`ctx.storage.sql` 复用同一套 node:sqlite 适配器；`ctx.acceptWebSocket`
/ `ctx.getWebSockets` 用一个数组当连接登记表；WebSocket 本体只实现
`send`/`serializeAttachment`/`deserializeAttachment` 三个方法。直接驱动
`fetch()`（鉴权 401/404、鉴权前置）和 `webSocketMessage()`（消息路由/
role 方向校验/配额降级）——这两个是 RoomDO 真正暴露给外界的入口，替身只
顶掉它们依赖的运行时基础设施，业务逻辑一行没改。

真正跑不进单测的只剩 Hibernation 生命周期本身（`new WebSocketPair()` 到
`return new Response(null, {status:101, webSocket:client})` 这一段握手
成功后的 101 响应），`src/room-do.js` 顶部注释里标了「集成测试待 wrangler
dev 手验」。
