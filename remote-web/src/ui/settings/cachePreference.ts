// cachePreference.ts — msgfix2 U4 · 「本设备缓存已加载的长消息全文」（body cache）开关的
// localStorage 偏好读写——同 `verbosePreference.ts` 的既有取向（per-device 偏好，不经协议、不落
// EventStore，见该文件头注）。
//
// **默认开（设计稿 §F/§4.2）**：body cache 只是加速层（省去重复 `msg.fetch` 网络往返），不是新增
// 的隐私暴露面——`EventStore` 本就持久化普通消息正文（既有暴露面，设计稿 §4.2 明确"本刀不迁移、不
// 在此开关承诺范围"），body cache 存的是同一批内容的"已加载全文"缓存，默认开启符合用户预期（不希望
// 每次重开 app 都要重新拉一遍刚看过的长消息）。关闭 = 不再写入 + 立即清空已缓存内容（调用方
// `app/AppRuntime.tsx::toggleCacheEnabled`）。
//
// **读写 try/catch，读不到默认开**（同 `verbosePreference.ts` 的降级取向，但默认值方向相反——
// verbose 默认关是"呈现克制"，body cache 默认开是"性能预期"，两者是独立的产品判断，不是同一条
// 规则的两个实例）。

const STORAGE_KEY = "agentloom.remote-web.settings.bodyCache";

export function loadCacheEnabledPreference(): boolean {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    // `null`（从未设置过——含全新安装/隐私模式清空过 storage）按默认开处理；显式写过 "0" 才是关。
    return raw === null ? true : raw === "1";
  } catch {
    return true;
  }
}

export function saveCacheEnabledPreference(next: boolean): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, next ? "1" : "0");
  } catch {
    // 静默降级——见文件头注，不能让存储异常打断当前交互。
  }
}
