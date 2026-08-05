import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import type { RepoMeta } from "./types/agent";

export type Locale = "zh" | "en";

const STORAGE_KEY = "agentloom.locale.v2";

export const localeOptions: {
  locale: Locale;
  label: string;
  native: string;
  short: string;
}[] = [
  { locale: "zh", label: "Chinese", native: "中文", short: "中" },
  { locale: "en", label: "English", native: "English", short: "EN" },
];

const messages = {
  zh: {
    "settings.title": "设置",
    "settings.close": "关闭",
    "settings.closeSettings": "关闭设置",
    "settings.version": "版本",
    "settings.nav.agents": "Agent 池",
    "settings.nav.search": "联网搜索",
    "settings.nav.repos": "仓库",
    "settings.nav.archivedProjects": "已归档项目",
    "settings.nav.language": "语言与区域",
    "settings.nav.defaults": "默认 & 模式",
    "settings.nav.allowlist": "namespace 白名单",
    "settings.nav.accounts": "账户 & Git",
    "settings.nav.budget": "成本 & 预算",
    "settings.nav.shortcuts": "快捷键",
    "settings.nav.about": "关于",
    "settings.about.support": "支持",
    "settings.about.feedback": "问题反馈",
    "settings.about.website": "官网",
    "aboutDialog.close": "关闭",
    "aboutDialog.copyVersion": "复制版本号",
    "aboutDialog.copied": "已复制",
    "aboutDialog.website": "官网",
    "aboutDialog.feedback": "问题反馈",
    "aboutDialog.support": "支持邮箱",
    "aboutDialog.copyright": "© 2026 MyAgentHubs",
    "archivedProjects.empty": "没有已归档的项目",
    "archivedProjects.restore": "恢复",
    "archivedProjects.deleteForever": "彻底删除",
    "archivedProjects.deleteConfirm.title": "彻底删除项目？",
    "archivedProjects.deleteConfirm.body":
      "彻底删除「{name}」及其所有会话（不可恢复）。磁盘上的代码不会被删除。",
    "archivedProjects.deleteConfirm.confirm": "彻底删除",
    "archivedProjects.deleteConfirm.cancel": "取消",
    "backend.project.cannotDeleteDefault": "默认项目「我的项目」不可删除。",
    "settings.search.intro":
      "不是每个模型都自带联网搜索。AgentLoom 接入第三方搜索服务，让任何 agent 都能查网页。DuckDuckGo 开箱即用、无需 key；配置 Brave 或 Exa API key 可获得更高质量的搜索结果。",
    "settings.search.formAriaLabel": "搜索服务设置",
    "settings.search.serviceLabel": "搜索服务",
    "settings.search.ddgNote": "DuckDuckGo 无需 API key。",
    "settings.search.useThisButton": "设为当前",
    "settings.search.useThisSwitching": "切换中…",
    "settings.search.useThisSwitched": "已切换为 DuckDuckGo。",
    "settings.search.useThisError": "切换失败，请稍后重试",
    "settings.search.apiKeyLabel": "API Key",
    "settings.search.testButton": "测试连接",
    "settings.search.testingButton": "测试中…",
    "settings.search.saveButton": "保存",
    "settings.search.savingButton": "保存中…",
    "settings.search.saveNote":
      "保存后 key 存入系统钥匙串，并将该服务设为当前使用。",
    "settings.search.saved": "已保存",
    "settings.search.saveError": "保存失败，请稍后重试",
    "settings.search.registerLink": "去 {label} 注册 key",
    "settings.search.placeholderConfigured": "已配置 · 粘贴新 key 可替换",
    "settings.search.placeholderEmpty": "粘贴 {apiName} Key",
    "settings.search.searxngComingSoon": "SearXNG（即将）",
    "settings.search.category.ok": "连接正常",
    "settings.search.category.auth": "key 无效或无权",
    "settings.search.category.rateLimit": "被限流·稍后再试",
    "settings.search.category.network": "网络错误",
    "settings.search.category.missingKey": "请先填 key",
    "settings.search.status.unknown": "未检查",
    "settings.search.status.checking": "检查中…",
    "settings.search.status.configured": "已配置",
    "settings.search.status.missing": "未配置",
    "settings.search.checkButton": "检查",
    "settings.language.title": "语言与区域",
    "settings.language.subtitle": "选择 AgentLoom 界面语言。",
    "settings.language.current": "当前语言",
    "settings.language.zh": "中文",
    "settings.language.en": "English",
    "settings.agentAccess.borrow": "经 Claude Code",
    "settings.agentAccess.harness": "内置引擎",
    "settings.agentAccess.native": "原生 CLI",
    "settings.agentKeyState.configured": "已配 ✓",
    "settings.agentKeyState.detected": "已检测到",
    "settings.agentKeyState.missing": "待配",
    "settings.agentKeyState.notInstalled": "未安装",
    "settings.agents.configuredCount": "已配置 {n} 个 agent",
    "settings.agents.description":
      "添加一个干活的 AI：选引擎、选模型、粘 key 就能用。",
    "settings.agents.add": "＋ 添加 agent",
    "settings.agents.empty": "暂无 agent",
    "settings.agents.listAria": "Agent 池列表",
    "settings.agents.providerModel.unset": "未设置 provider/model",
    "settings.agents.nativeAutoDetectTitle": "随本机 CLI 自动接入·无需配置",
    "settings.agents.nativeAutoDetect": "自动检测",
    "settings.agents.edit": "编辑",
    "settings.agents.delete": "删除",
    "settings.agents.deleteAria": "删除 {name}",
    "onboarding.installGuide.title": "还没有可用的 agent",
    "onboarding.installGuide.reason":
      "AgentLoom 可以用内置引擎 myagent 跑 agent —— 只需要你自己的 API key，不用装任何厂商 CLI；也可以驱动本机的 Claude Code 或 Codex CLI。当前三种都还没配置好。",
    "onboarding.installGuide.harnessDescription":
      "用你自己的 API key 直接跑，不需要安装任何厂商 CLI。",
    "onboarding.installGuide.configureHarness": "去配置",
    "onboarding.installGuide.claudeDescription":
      "使用 Anthropic 账号运行 Claude agent。",
    "onboarding.installGuide.codexDescription":
      "使用 OpenAI 账号运行 Codex agent。",
    "onboarding.installGuide.openInstallGuide": "打开安装指引",
    "onboarding.installGuide.openSettings": "打开 Agent 设置",
    "onboarding.installGuide.dismiss": "稍后再说",
    "settings.agentForm.category.auth": "key 无效或无权限",
    "settings.agentForm.category.rateLimit": "额度不足或频控（非地址问题）",
    "settings.agentForm.category.network": "连不上 endpoint，检查网络/地址",
    "settings.agentForm.category.notFound": "endpoint 或模型不存在",
    "settings.agentForm.category.missingKey": "请粘贴 API Key 后测试",
    "settings.agentForm.category.endpointRequired": "请先填写 endpoint",
    "settings.agentForm.category.other": "请求失败",
    "settings.agentForm.group.account": "账号",
    "settings.agentForm.saveBlocked.nativeMissing":
      "未安装 {cli} CLI，暂不能保存",
    "settings.agentForm.saveBlocked.testFailed": "测试未通过，暂不能保存",
    "settings.agentForm.engineStatus.builtIn": "✓ 内置 · 免安装",
    "settings.agentForm.engineStatus.installedLoggedIn": "✓ 已安装 · 已登录",
    "settings.agentForm.engineStatus.installed": "✓ 已安装",
    "settings.agentForm.engineStatus.notDetected": "⚠ 未检测到 ·",
    "settings.agentForm.engineStatus.installGuide": "安装指引",
    "settings.agentForm.engineStatus.installGuideAria": "{engine} 安装指引",
    "settings.agentForm.nativeStatus.loggedIn":
      "✓ {cli} CLI 已检测 · 已登录，用你的 {account} 账号，无需 API Key",
    "settings.agentForm.nativeStatus.installedNoCredsPrefix":
      "⚠ {cli} CLI 已安装，未探测到登录凭据；可先保存，若运行报错，在终端跑",
    "settings.agentForm.nativeStatus.installedNoCredsSuffix": "后点",
    "settings.agentForm.nativeStatus.notDetected": "⚠ 未检测到 {cli} CLI",
    "settings.agentForm.nativeStatus.recheck": "重新检测",
    "settings.agentForm.nativeStatus.viewInstallGuide": "查看安装指引",
    "settings.agentForm.moreSummary.borrow":
      "模型 · reasoning · endpoint · 鉴权 · 模型映射 · 超时 · 兼容性",
    "settings.agentForm.moreSummary.harness":
      "模型 · reasoning · endpoint · 超时",
    "settings.agentForm.moreSummary.native": "模型 · reasoning",
    "settings.agentForm.modelLabel": "模型",
    "settings.agentForm.primaryModelLabel": "主模型",
    "settings.agentForm.harnessModelPlaceholder": "留空 = myagent 默认",
    "settings.agentForm.harnessDefaultModelOption": "myagent 默认",
    "settings.agentForm.fromList": "↩ 从列表选择",
    "settings.agentForm.unknownModelWarning":
      "未识别的模型 id——请核对拼写（如 claude-fable-5）。仍可保存，但该 agent 可能无法启动。",
    "settings.agentForm.modelPlaceholder.cliDefault": "CLI 默认",
    "settings.agentForm.modelPlaceholder.select": "选择模型",
    "settings.agentForm.reasoningLabel": "reasoning 默认档",
    "settings.agentForm.reasoningDisabledHint": "未启用 reasoning 档位",
    "settings.agentForm.authLabel": "鉴权方式",
    "settings.agentForm.autoMark": "· 自动",
    "settings.agentForm.modelMappingLabel": "模型映射",
    "settings.agentForm.modelMappingHint":
      "关键：Claude Code 用 haiku 档跑后台杂活/subagent，该 endpoint 没有 claude-haiku 时必须映射到该家小模型，否则后台任务会 400/404。",
    "settings.agentForm.maxOutputTokensPlaceholder":
      "默认：跟随模型上限（推荐不改）",
    "settings.agentForm.compatLabel": "兼容性开关（借壳细调 · 一般不动）",
    "settings.agentForm.compatDisableThinking": "关 thinking",
    "settings.agentForm.compatDisableBetas": "关 betas",
    "settings.agentForm.compatDisableNonessential": "关非必要流量",
    "settings.agentForm.compatProxyPlaceholder": "如 thinking_passback",
    "settings.agentForm.formAria": "添加 / 编辑 agent",
    "settings.agentForm.title.add": "添加 agent",
    "settings.agentForm.title.edit": "编辑 agent",
    "settings.agentForm.borrowIntro":
      "有自己的 CLI 就用原生（claude/codex）；其余 provider 自动经 Claude Code 接入，你只需粘 key。",
    "settings.agentForm.basic": "基础",
    "settings.agentForm.engineLabel": "引擎",
    "settings.agentForm.engineDesc.claudeCode":
      "本机 claude 命令。可跑 Anthropic 自家，也可借壳跑别家",
    "settings.agentForm.engineDesc.codex":
      "本机 codex 命令。跑 OpenAI 自家模型",
    "settings.agentForm.engineDesc.myagent": "自研 harness，直连各家 API",
    "settings.agentForm.presetLabel.custom": "自定义",
    "settings.agentForm.providerUpcoming": "借壳 · 后续版本",
    "settings.agentForm.accountChip": "{account} 账号",
    "settings.agentForm.cliLoggedIn": "CLI 已登录",
    "settings.agentForm.accessPointLabel": "接入点",
    "settings.agentForm.accessPoint.default": "默认",
    "settings.agentForm.accessPoint.cn": "中国",
    "settings.agentForm.accessPoint.intl": "国际",
    "settings.agentForm.accessPoint.cn-coding": "中国 · Coding 套餐",
    "settings.agentForm.accessPoint.intl-coding": "国际 · Coding 套餐",
    "settings.agentForm.borrowPresetHint":
      "选好 Provider 预设后会自动带入模型映射 / 鉴权方式 / 超时 / 兼容开关；需要 key 的 provider 粘 API Key 即可。",
    "settings.agentForm.harnessHint":
      "myagent 直连该 provider（OpenAI 兼容），选模型（可留空用默认）+ 粘 key 即可；endpoint 已内置，无需模型映射/鉴权方式。",
    "settings.agentForm.nameLabel": "名称",
    "settings.agentForm.apiKeyHint": "存本地 keychain · 不上传 · 仅本机",
    "settings.agentForm.existingKeyPlaceholder": "已配置 · 留空保留原 key",
    "settings.agentForm.showApiKey": "显示 API Key",
    "settings.agentForm.hideApiKey": "隐藏 API Key",
    "settings.agentForm.testing": "测试中…",
    "settings.agentForm.testConnection": "测试连接",
    "settings.agentForm.keyStatusPrefix": "Key 状态：",
    "settings.agentForm.keepStoredKeyHint": "；留空不修改已存 key",
    "settings.agentForm.borrowKeyMissing": "未配 key · 该 agent 暂不可用",
    "settings.agentForm.multiAp.keyHint":
      "{accessPoints} 的 key 不通用 · 当前请用 {keyHint} 申请的 key",
    "settings.agentForm.multiAp.noKeyHint":
      "{accessPoints} 的 key 不通用 · 当前请用当前接入点的 key",
    "settings.agentForm.testSuccess": "连接成功",
    "settings.agentForm.testSuccessFetchedHarness": " · 已拉到 {n} 个模型",
    "settings.agentForm.testSuccessFetchedBorrow": " · 已拉取 {n} 个模型",
    "settings.agentForm.rawErrorToggle": "展开原始错误",
    "settings.agentForm.moreOptions": "更多选项",
    "settings.agentForm.runModeLabel": "运行方式",
    "settings.agentForm.runModeHint": "由 provider 预设自动决定，无需手选",
    "settings.agentForm.cancel": "取消",
    "settings.agentForm.save": "保存",
    "settings.agentForm.add": "添加",
    "settings.agentForm.error.nameRequired": "名称必填",
    "settings.agentForm.error.primaryModelRequired": "主模型必填",
    "settings.agentForm.error.endpointRequired": "该 agent 需要填写 Endpoint",
    "settings.agentForm.error.saveFailed": "保存失败，请稍后重试",
    "settings.modelDropdown.custom": "自定义…（手敲 model 名）",
    "settings.modelDropdown.placeholder": "选择模型",
    "settings.modelDropdown.live": "实时",
    "overview.title": "总览",
    "overview.subtitle": "跨 repo 会话舰队",
    "overview.sessionStats": "会话统计",
    "overview.empty":
      "还没有会话。用左下项目切换器选一个项目，开始第一条会话。",
    "overview.needsAttention": "需注意",
    "overview.running": "运行中",
    "overview.idle": "闲置",
    "overview.repoCount": "仓库",
    "overview.summary":
      "{attention} 个会话需要你 · {running} 个在跑 · 跨 {repos} 个仓库",
    "overview.summaryAttentionSuffix": "个会话需要你",
    "overview.summaryRunningSuffix": "个在跑",
    "overview.summaryRepos": "跨 {repos} 个仓库",
    "overview.actionBand": "现在要你处理",
    "overview.recapBand": "最近怎么样",
    "overview.allClear": "一切正常，没有会话在等你。",
    "overview.expandMore": "展开其余 {n} 条",
    "overview.collapse": "收起",
    "overview.localDefault": "Local 默认",
    "overview.unknownRepo": "未知仓库",
    "overview.team": "Agent Team",
    "overview.normal": "Normal",
    "overview.signal.pending": "待处理",
    "overview.signal.running": "运行中",
    "overview.signal.recent": "最近",
    "overview.folded": "折叠",
    "overview.activity": "最近活动",
    "overview.activityEmpty": "最近还没有改动记录。",
    "overview.activityError": "最近活动加载失败，请稍后再试。",
    "overview.activityCommits": "{n} 次改动",
    "overview.activityFailed": "{n} 次失败",
    "overview.activityChartAria":
      "最近活动按天柱状图，柱高表示每天的增删行数。",
    "overview.activityTooltip":
      "{date}：{commits} 次提交 / +{insertions} −{deletions} 行",
    "overview.activityTooltipFailed":
      "{date}：{commits} 次提交 / +{insertions} −{deletions} 行 / 失败 {failed} 次",
    "overview.activityFailureLegend": "红点表示当天有失败提交",
    "overview.usage": "用量",
    "overview.usageHint": "输入 + 输出总量；带缓存命中的输入 token 会低报。",
    "overview.usageTooltip": "{project}：{tokens} tokens，占总量 {percent}%",
    "overview.usageEmpty": "还没有用量数据。",
    "overview.usageSessions": "{n} 个会话",
    "overview.usageTopSessions": "用量最高的会话",
    "projectIntro.defaultSession": "默认会话",
    "projectIntro.defaultPath": "无关联项目 · 工作目录由 AgentLoom 自动管理",
    "projectIntro.title": "项目简介",
    "projectIntro.defaultPlaceholder":
      "默认会话还没有项目简介。AI 会在你与它对话过程中自动整理一个简短摘要到这里。",
    "projectIntro.tabsAria": "项目简介 tab",
    "projectIntro.tabIntro": "项目简报",
    "projectIntro.tabDaily": "Daily",
    "projectIntro.rendered": "渲染",
    "projectIntro.source": "原文",
    "projectIntro.aiAnalysis": "AI 解析",
    "projectIntro.repoPlaceholder":
      "还没有 README.md。AI 会在你与它对话过程中自动整理这个项目的简介到这里。",
    "repoDoc.empty.title.intro": "还没有解析过这个项目",
    "repoDoc.empty.desc.intro":
      "让一个 agent 通读这个代码库，生成一份能快速看懂它的说明：它是什么、技术栈、目录结构要点、最近在做什么。",
    "repoDoc.empty.cta.intro": "开始 AI 解析",
    "repoDoc.empty.title.daily": "还没有今天的日报",
    "repoDoc.empty.desc.daily":
      "让 agent 汇总近期 commit、会话产出与 token 概况，生成今天的项目日报。",
    "repoDoc.empty.cta.daily": "生成今日日报",
    "repoDoc.readonly": "只读 · agent 只读取和搜索，不会修改你的任何文件",
    "repoDoc.generating.title.intro": "正在解析项目",
    "repoDoc.generating.title.daily": "正在生成今日日报",
    "repoDoc.generating.lede": "可离开本页，生成会在后台继续",
    "repoDoc.stale": "基于 {sha} 生成 · 仓库已有新提交 · 可刷新",
    "repoDoc.generatedAt": "生成于",
    "repoDoc.disclaimer": "AI 生成 · 可能有误",
    "repoDoc.commit": "基于 commit {sha}",
    "repoDoc.error": "生成失败",
    "repoDoc.retry": "重试",
    "repoDoc.regenerate": "重新生成",
    "repoDoc.loading": "正在载入文档…",
    "files.emptyTitle": "进入会话后浏览项目文件",
    "files.emptyDesc": "Files 会显示当前会话所属 workspace 的全量项目树。",
    "files.rendered": "查看渲染",
    "files.source": "查看源码",
    "files.find": "Find in file",
    "files.copyPath": "Copy path",
    "files.showTree": "展开文件树",
    "files.hideTree": "收起文件树",
    "files.findPlaceholder": "Find in file…",
    "files.prevMatch": "上一个命中",
    "files.nextMatch": "下一个命中",
    "files.loading": "Loading files…",
    "files.noPreview": "没有可预览的文本文件",
    "files.filterPlaceholder": "Filter files…",
    "files.directory": "目录 {path}",
    "files.open": "打开 {path}",
    "files.expandDirectory": "展开目录 {path}",
    "files.collapseDirectory": "折叠目录 {path}",
    "files.openFile": "打开文件 {path}",
    "files.truncated": "项目条目较多，仅显示前 {max} 项",
    "preview.empty": "选择一个文件预览",
    "preview.loading": "加载中…",
    "preview.error": "无法打开",
    "preview.truncated": "已截断至 256 KB",
    "preview.imageUnavailable": "图片无法预览",
    "preview.binary": "二进制文件，无法预览",
    "rightPanel.soon.side": "Side chat（与成员/旁路 agent 单独对话）即将到来。",
    "rightPanel.soon.terminal": "Terminal（交互式 shell）即将到来。",
    "rightPanel.soon.browser": "Browser（打开网页）即将到来。",
    "rightPanel.picker.hint": "选一个工具开成 tab",
    "rightPanel.picker.open": "打开 {name}",
    "rightPanel.picker.unavailable": "{name} 即将支持",
    "rightPanel.picker.soon": "即将",
    "rightPanel.picker.previewLabel": "预览",
    "rightPanel.picker.filesDescription": "浏览整个项目文件 · 点文件看内容",
    "rightPanel.picker.reviewDescription": "查看当前改动 diff",
    "rightPanel.picker.sideDescription": "开一段旁路对话",
    "rightPanel.picker.terminalDescription": "开一个交互式 shell",
    "rightPanel.picker.browserDescription": "打开一个网页",
    "rightPanel.preview.close": "关闭预览",
    "rightPanel.empty.noSessionTitle": "进入会话后审查",
    "rightPanel.empty.noChangesTitle": "尚无改动",
    "rightPanel.empty.noSessionDescription":
      "打开或新建一个会话后，agent 的改动会出现在这里待审。",
    "rightPanel.empty.noChangesDescription":
      "这个会话还没有待审的 agent 改动。agent 改完文件后会出现在这里。",
    "rightPanelTabs.expand": "展开右面板",
    "rightPanelTabs.expandTitle": "展开右面板 ⌘J",
    "rightPanelTabs.newTab": "新 tab / 回选择器",
    "rightPanelTabs.restore": "恢复分栏",
    "rightPanelTabs.restoreTitle": "恢复分栏（右面板返回侧栏）",
    "rightPanelTabs.maximize": "展开（占用 main）",
    "rightPanelTabs.maximizeTitle":
      "展开（右面板占用 main 区域，sidebar 保留）",
    "rightPanelTabs.collapse": "收起右面板",
    "rightPanelTabs.collapseTitle": "收起右面板 ⌘J",
    "reviewPanel.title": "改动 · {count} 文件",
    "reviewPanel.close": "关闭",
    "reviewPanel.noUndoRecord": "未留撤销记录 · 退不回",
    "reviewPanel.dataFileNotShown": "数据文件 · 不显示内容",
    "reviewPanel.showMore": "显示更多",
    "reviewPanel.statusCommittedAndUncommitted":
      "已提交 {committed} 个文件 · 未提交 {uncommitted} 个文件",
    "reviewPanel.statusCommittedOnly": "已提交 {committed} 个文件",
    "reviewPanel.statusUncommittedOnly": "未提交 {uncommitted} 个文件",
    "reviewPanel.unavailableTitle": "无法生成改动对比",
    "reviewPanel.unavailableDescription":
      "这个项目还不是带 HEAD 的 Git 工作树。先做一次提交（commit），这里就能看到改动对比了。",
    "reviewPanel.otherDirty": "工作目录另有 {count} 个未纳入本次 Review 的变更",
    "undoPanel.checklist.aria": "这一轮的撤销清单",
    "undoPanel.checklist.title": "这一轮的改动 · {count} 个文件",
    "undoPanel.checklist.mode": "只看这一轮",
    "undoPanel.result.aria": "这一轮的撤销结果",
    "undoPanel.result.title": "撤销结果 · {count} 个文件",
    "undoPanel.result.mode": "撤销之后",
    "undoPanel.result.subtitle": "本轮逐文件撤销结果",
    "undoPanel.back": "退出只看这一轮",
    "undoPanel.loading": "正在读取这一轮的改动…",
    "undoPanel.loadFailed": "无法读取撤销清单：{reason}",
    "undoPanel.empty": "这一轮没有可撤销的编辑工具改动。",
    "undoPanel.allStale":
      "这一轮记录已全部过期：改过的文件后来又被提交，撤销会覆盖那些提交，所以都不能选。",
    "undoPanel.selectFile": "选择 {path}",
    "undoPanel.kind.created": "新建",
    "undoPanel.kind.modified": "修改",
    "undoPanel.kind.deleted": "删除",
    "undoPanel.file.modified": "修改 · 撤销后恢复为本轮改动前的内容",
    "undoPanel.file.created": "新建 · 撤销会删除这个文件",
    "undoPanel.file.deleted": "删除 · 撤销会恢复这个文件",
    "undoPanel.file.binary": "二进制文件，无法预览 · 仍可勾选撤销",
    "undoPanel.file.tooLarge": "文件过大（{size}），无法预览 · 仍可勾选撤销",
    "undoPanel.file.unsupported": "无法预览 · 仍可勾选撤销",
    "undoPanel.file.alreadyUndone": "已经撤销 · 不可再次选择",
    "undoPanel.file.stale":
      "这条记录已过期：文件在此之后又被提交，撤销会覆盖之后的提交 · 不可选择",
    "undoPanel.badge.binary": "二进制",
    "undoPanel.diff.modified": "改动前 → 现在",
    "undoPanel.diff.created": "改动前不存在 → 现在",
    "undoPanel.diff.deleted": "改动前 → 现在不存在",
    "undoPanel.boundary.title": "撤销只覆盖 agent 用编辑工具改的文件。",
    "undoPanel.boundary.terminalPrefix": "agent 在终端里干的事（",
    "undoPanel.boundary.rm": "rm",
    "undoPanel.boundary.separator": " / ",
    "undoPanel.boundary.sed": "sed -i",
    "undoPanel.boundary.terminalSuffix":
      " / 脚本 / 重定向）不在此列，退不回来。",
    "undoPanel.undoSelected": "撤销选中的 {count} 个文件",
    "undoPanel.undoing": "正在撤销…",
    "undoPanel.undoFailed": "撤销失败：{reason}",
    "undoPanel.result.restored": "已还原 {count} 个文件",
    "undoPanel.result.skipped": "未还原 {count} 个文件",
    "undoPanel.result.skippedDetail":
      "请查看每个文件的具体原因；当前内容没有被覆盖。",
    "undoPanel.result.failed": "失败 {count} 个",
    "undoPanel.result.file.restored": "已撤销 · 已恢复为本轮改动前的内容",
    "undoPanel.result.file.createdRestored": "已撤销 · 新建文件已删除",
    "undoPanel.result.file.deletedRestored": "已撤销 · 已删除文件已恢复",
    "undoPanel.result.file.skippedChanged":
      "未还原 · 你查看后它又变了，没有还原",
    "undoPanel.result.file.skippedUnsafe":
      "未还原 · 这个路径现在无法安全访问，没有还原",
    "undoPanel.result.file.skippedAlreadyUndone": "之前已经撤销过了",
    "undoPanel.result.file.skippedStale":
      "未还原 · 这条记录已过期，撤销会覆盖之后的提交",
    "undoPanel.result.file.skippedUnknown": "未还原 · 后端原因：{reason}",
    "undoPanel.result.file.failed": "未还原 · 撤销失败：{reason}",
    "undoPanel.result.badge.restored": "已还原",
    "undoPanel.result.badge.deleted": "已删除",
    "undoPanel.result.badge.skipped": "未还原",
    "undoPanel.result.badge.failed": "失败",
    "undoPanel.result.changedDiff": "撤销前复核发现内容已变化",
    "inlineDiffCard.openInReview": "在 Review 里打开",
    "stream.role.lead": "队长",
    "stream.role.user": "你",
    "quote.role.assistant": "助手",
    "stream.status.workingAria": "{name} 正在工作",
    "stream.status.working": "工作中",
    "stream.status.lastStep": "上一步：{summary}",
    "stream.status.thinking": "思考中",
    "stream.status.silent": "已静默 {seconds}s",
    "stream.status.longTask": "引擎长任务运行中",
    "stream.status.waitingOnWorker": "等待 worker：{name}",
    "stream.status.waitingOnWorkers": "等 {count} 个 worker：{name} 等",
    "stream.task.view": "查看",
    "stream.worker.badge.stopped": "已中断",
    "runCard.state.undone": "已撤销本轮",
    "runCard.state.partial": "已撤销 {undone} / {total}",
    "runCard.state.completed": "已完成",
    "runCard.changesAria": "本轮改动",
    "runCard.summary": "本轮改了 {files} 文件",
    "runCard.interrupted": " · 中断",
    "runCard.view": "查看",
    "runCard.undo": "撤销…",
    "runCard.continueUndo": "继续撤销…",
    "runCard.viewResult": "查看结果",
    "runCard.result.restored": "本次已还原 {count} 个",
    "runCard.result.skipped": "本次未还原 {count} 个",
    "runCard.result.failed": "本次失败 {count} 个",
    "runCard.result.unselected": "本次未选择 {count} 个",
    "runCard.partialNote.both":
      "本次未还原 {skipped} 个、失败 {failed} 个；请查看各文件的具体原因",
    "runCard.partialNote.skipped":
      "本次有 {count} 个文件未还原，请查看结果中的具体原因",
    "runCard.partialNote.failed": "本次有 {count} 个文件撤销失败，请查看结果",
    "runLeadTurn.fallbackLeadName": "队长",
    "runLeadTurn.captain": "· 队长",
    "runLeadTurn.processSummary": "过程：{count} 个任务",
    "runLeadTurn.viewProcess": "查看过程",
    "taskStack.undoRun": "撤销这一轮",
    "liveStreamCard.running": "执行中",
    "liveStreamCard.preparing": "准备中…",
    "liveStreamCard.isolated": "隔离区",
    "memberDrillIn.noTokens": "无 token",
    "memberDrillIn.status.running": "进行中",
    "memberDrillIn.status.needsInput": "等你确认",
    "memberDrillIn.status.done": "已完成",
    "memberDrillIn.status.failed": "失败",
    "memberDrillIn.status.stopped": "已停止",
    "memberDrillIn.criterion.pending": "待验",
    "memberDrillIn.criterion.passed": "通过",
    "memberDrillIn.criterion.failed": "没过",
    "memberDrillIn.criterion.waived": "跳过",
    "memberDrillIn.criterion.uncertain": "待确认",
    "memberDrillIn.backToLead": "回 Lead",
    "memberDrillIn.steps": "步 {done}/{total} · {tokens}",
    "memberDrillIn.stopAria": "停止 {name}",
    "memberDrillIn.stop": "⏹停",
    "memberDrillIn.failureReason": "失败原因",
    "memberDrillIn.overview": "概览",
    "memberDrillIn.taskDetails": "任务详情",
    "memberDrillIn.goal": "目标",
    "memberDrillIn.acceptance": "验收",
    "memberDrillIn.changedFiles": "改动文件",
    "memberDrillIn.changedFilesCaveat":
      "改动文件清单来自 checkpoint 账本;agent 在终端直写(shell 重定向 / sed 等)可能未入账。",
    "memberDrillIn.verification": "验证",
    "memberDrillIn.exitCode": "退出码 {code}",
    "memberDrillIn.viewAssignment": "查看派单",
    "memberDrillIn.rawTrace": "原始过程",
    "messageContent.image": "[图片]",
    "messageContent.imageLoading": "正在加载图片…",
    "messageContent.imageLoadFailed": "[图片加载失败]",
    "messageContent.imageArtifact.preview": "预览图片 {name}",
    "messageContent.imageMenu.label": "图片操作",
    "messageContent.imageMenu.copyImage": "复制图片",
    "messageContent.imageMenu.copyPath": "复制全路径",
    "messageContent.imageMenu.imageUnavailable": "当前环境不支持复制图片",
    "messageContent.imageMenu.imageCopied": "图片已复制",
    "messageContent.imageMenu.pathCopied": "路径已复制",
    "messageContent.imageMenu.copyFailed": "复制失败",
    "messageContent.html.openExternal": "在浏览器打开 {name}",
    "lightbox.label": "图片放大预览",
    "lightbox.close": "关闭图片预览",
    "lightbox.imageAlt": "放大的图片",
    "lightbox.loading": "正在加载大图…",
    "lightbox.loadFailed": "图片加载失败",
    "messageContent.gate.proposing": "Lead 正在拟计划…",
    "messageActions.copied": "已复制",
    "messageActions.copy": "复制",
    "messageActions.exportMarkdown": "导出 markdown",
    "messageActions.quote": "引用",
    "messageMarkdown.toolStatusWithExit": "[{status} exit {exitCode}]",
    "messageMarkdown.toolStatus": "[{status}]",
    "messageMarkdown.image": "![image](attachment:{attachmentId})",
    "messageMarkdown.teamRun": "[Agent Team · {n} 个子任务（{names}）]",
    "messageMarkdown.runCard":
      "[本轮改动 {n} 文件 (+{insertions} −{deletions})]",
    "messageMarkdown.approval": "[审批 {status}：{tool} · {command}]",
    "messageMarkdown.scopeChange": "[agent 提议改任务范围]",
    "messageMarkdown.leadSummary": "[Lead 汇总 · {source}]",
    "messageMarkdown.codingTask": "[coding task · {phase}]",
    "messageMarkdown.gateCard": "[计划草案]",
    "messageMarkdown.draftFailed": "[拟失败]",
    "messageMarkdown.dispatchCard": "\n[任务：{name} · {sub}]\n",
    "messageMarkdown.decisionCard": "[决策卡]",
    "messageMarkdown.runTerminalWithMessage": "[{status} · {message}]",
    "messageMarkdown.runTerminal": "[{status}]",
    "thinking.collapse": "收起",
    "thinking.expand": "展开",
    "codeBlock.openInBrowser": "在浏览器打开",
    "codeBlock.openTemporaryHtml": "打开临时 HTML",
    "codeBlock.copied": "已复制",
    "codeBlock.copy": "复制",
    "codeBlock.collapse": "收起",
    "codeBlock.expandLines": "展开 +{n} 行",
    "toolCard.status.running": "运行中",
    "toolCard.status.done": "完成",
    "toolCard.status.failed": "失败",
    "toolCard.status.interrupted": "已中断",
    "toolCard.hiddenLinesAbove": "+ {n} 行（上方）",
    "toolCard.name.bash": "运行命令",
    "toolCard.name.read": "读文件",
    "toolCard.name.write": "写文件",
    "toolCard.name.edit": "改文件",
    "toolCard.name.glob": "找文件",
    "toolCard.name.grep": "搜索",
    "toolCard.name.task": "子任务",
    "toolCard.name.todoWrite": "整理待办",
    "toolCard.name.webFetch": "抓网页",
    "toolCard.name.webSearch": "搜网页",
    "toolCard.name.notebookEdit": "改笔记本",
    "toolCard.name.bashOutput": "看命令输出",
    "toolCard.name.killShell": "停止命令",
    "toolCard.name.ls": "列目录",
    "toolCard.name.memory": "记笔记",
    "toolCard.name.imageGen": "生成图片",
    "toolCard.name.commit": "提交代码",
    "toolCard.name.push": "推送代码",
    "toolCard.name.createPr": "创建 PR",
    "toolCard.name.publish": "发布",
    "toolCard.name.verifier": "验证",
    "inspector.status": "状态",
    "inspector.owner": "执行者",
    "inspector.artifacts": "产物",
    "inspector.failureReason": "失败原因",
    "inspector.stderrTail": "错误输出（末尾）",
    "inspector.toolTrace": "过程",
    "inspector.noOutput": "尚无产出",
    "inspector.close": "关闭",
    "inspector.statusLabel.running": "进行中",
    "inspector.statusLabel.needs_input": "等你确认",
    "inspector.statusLabel.done": "已完成",
    "inspector.statusLabel.failed": "失败",
    "inspector.statusLabel.stopped": "已停止",
    "inspector.filesUnit": "{n} 个文件",
    "stream.toolFold.steps": "执行了 {n} 步",
    "runTerminal.completed": "已完成",
    "runTerminal.error": "出错",
    "runTerminal.interrupted": "已中断",
    "runTerminal.blocked": "已停下",
    "runTerminal.needsDecision": "待决策",
    "runTerminal.fallback": "会话收尾未完成 · 已兜底恢复现场",
    "stopReason.blockedQuestions": "lead 停在待决问题上",
    "stopReason.noProgress": "连续多轮没有实质进展，已自动停下",
    "stopReason.stuckRepeating": "重复同样操作被安全网停下",
    "stopReason.budgetExhaustedStillProgressing":
      "本轮回合预算用完（任务还在推进）——发一条消息可继续",
    "stopReason.contextBudgetExhausted": "上下文用满，已收工——发一条消息可继续",
    "stopReason.approvalUnavailable": "引擎批准通道不可用，已停下",
    "stopReason.rejectedRepeatedly": "多次提交未过验收，已停下",
    "app.run.stoppedPendingQuestion":
      "\n还有问题在等你回答，点上方选项即可继续。",
    "time.justNow": "刚刚",
    "time.minAgo": "{n} 分钟前",
    "time.hourAgo": "{n} 小时前",
    "time.dayAgo": "{n} 天前",
    "codingTask.phase.finalizing": "固化改动",
    "codingTask.phase.askVerify": "待确认验证命令",
    "codingTask.phase.verifying": "验证中",
    "codingTask.phase.verifyFailed": "验证未通过",
    "codingTask.phase.askApply": "旧落地确认",
    "codingTask.phase.merging": "合并至暂存",
    "codingTask.phase.applying": "待你决定",
    "codingTask.phase.applied": "已落地",
    "codingTask.phase.landingBlocked": "已阻止",
    "codingTask.phase.shelved": "已搁置",
    "codingTask.phase.error": "出错",
    "taskStatus.chip.steps": "{done}/{total}",
    "taskStatus.chip.files": "{n} files",
    "taskStatus.chip.verify": "{n} 验证",
    "taskStatus.phase.askApplyProgress": "旧落地确认（请重新运行或先放着）",
    "taskStatus.phase.applyingProgress": "改动在隔离区",
    "taskStatus.phase.landingBlockedProgress": "落地前检查未通过",
    "codingTask.why": "为什么",
    "codingTask.details": "详情",
    "codingAsk.verifyFailed": "验证没通过",
    "codingAsk.command": "命令",
    "codingAsk.retryWithCommand": "改命令重验",
    "codingAsk.viewChanges": "查看改动",
    "codingAsk.shelve": "先放着",
    "codingAsk.verifyPrompt": "建议用以下命令验证这次改动，可修改：",
    "codingAsk.startVerify": "开始验证",
    "scopeChange.kind.scope": "范围",
    "scopeChange.kind.objective": "目标",
    "scopeChange.kind.constraint": "约束",
    "scopeChange.continueDraft": "接上一轮，采纳以下范围调整：\n{changes}",
    "scopeChange.collapsedTitle": "agent 曾提议改任务范围",
    "scopeChange.collapsedStatus": "已收起",
    "scopeChange.expand": "展开查看提议内容",
    "scopeChange.title.multi": "agent 提议调整任务边界（{count} 条）",
    "scopeChange.title.single": "agent 提议改任务范围",
    "scopeChange.pending": "等你决定",
    "scopeChange.description.multi":
      "agent 一次提了 {count} 条边界调整，等你拍板。",
    "scopeChange.description.single":
      "agent 干到一半停下，提议调整这次任务的边界，等你拍板。你可以按它的建议接着干，或者收起、按你自己的想法另说。",
    "scopeChange.finalizeNote":
      "这一轮到此结束，可能已经改过一些文件（已照常存档）；范围调整还没真正动手。",
    "scopeChange.continueHint":
      "「采纳并继续」会按 agent 提议的边界，直接发起下一轮。",
    "scopeChange.collapse": "收起",
    "scopeChange.acceptAndContinue": "采纳并继续",
    "composer.permission.label": "权限：Auto · 当前版本先只 Auto",
    "composer.permission.trustBase": "信任落地·事后可在 Review 查看/撤销",
    "composer.permission.autoOnly": "当前版本先只 Auto",
    "composer.permission.shortLabel": "权限",
    "composer.attachment.label": "附加文件",
    "composer.attachment.remove": "移除附件",
    "composer.attachment.imageAlt": "附加图片",
    "composer.attachment.comingSoon": "还在计划中",
    "composer.voice.label": "语音",
    "composer.voice.comingSoon": "还在计划中",
    "composer.usage.total": "全程",
    "composer.readonly.continued": "会话已交接到新会话·只读·请到新会话继续",
    "composer.quote.clear": "清除引用",
    "composer.pendingDecision.label": "有一件事等你确认",
    "composer.input.placeholder": "输入消息…",
    "composer.stop": "停止",
    "composer.send": "发送",
    "composer.status.membersWorking": "队员工作中…",
    "composer.memberActiveHint":
      "成员任务仍在运行，等它完成或在卡片上停止后再发送",
    "composer.memberRecheckFailedHint": "无法确认成员任务状态，请稍后重试",
    "composer.hint.send": "Enter 发送 · Shift+Enter 换行",
    "composer.agentSelector.loadingSuffix": "，加载中",
    "composer.agentSelector.trigger.team":
      "选择 agent：队长 {name}，成员 {count}{loading}",
    "composer.agentSelector.trigger.solo": "选择 agent：{name}{loading}",
    "composer.agentSelector.description.canLead": "{provider} · 可带队 + 调度",
    "composer.agentSelector.description.unavailable": "{provider} · 仅可当队员",
    "composer.agentSelector.role.lead": "队长",
    "composer.agentSelector.members.count": "成员 {count}",
    "composer.agentSelector.title.team": "这个会话用谁",
    "composer.agentSelector.title.solo": "选择 agent",
    "composer.agentSelector.auto.teamUnavailableTitle":
      "Auto 只服务 Solo 自动选 · 已设队长（Team）时不可用",
    "composer.agentSelector.auto.teamUnavailable":
      "Team 模式下不可用（取消队长后恢复）",
    "composer.agentSelector.empty": "暂无可用 agent · 去 Settings 配置",
    "composer.agentSelector.action.cancelLead": "取消队长 {name}",
    "composer.agentSelector.action.setLead": "设为队长 {name}",
    "composer.agentSelector.memberAria": "成员 {name}",
    "composer.agentSelector.foot":
      "皇冠 = 设队长（单选·排它），成员开关决定 worker 名单。取消队长 = 回 Solo。",
    "composer.agentSelector.manage": "管理 agent →",
    "teamBar.roleLead": "Lead",
    "teamBar.barMembers": "成员 {n} 名",
    "teamBar.barRunning": "{running} 名成员在跑 · 共 {total}",
    "teamBar.expand": "展开",
    "teamBar.collapse": "收起",
    "teamBar.panelTitle": "组队配置 · 会话级（改动粘滞本会话）",
    "teamBar.leadHead": "Lead（带队 · 主对话伙伴）",
    "teamBar.leadCantBe": "暂不能当 Lead · 可当成员",
    "teamBar.rosterHead": "成员名单（可参与的人 · Lead 按需从中挑 · 不全员上）",
    "teamBar.memberAria": "成员 {name}",
    "teamBar.capHint.claude": "带队 / 通用",
    "teamBar.capHint.codex": "实现 / 测试",
    "teamBar.capHint.gemini": "多模态 / 文案",
    "teamBar.capHint.kimi": "长文总结",
    "teamBar.capHint.deepseek": "快搜 / 低成本",
    "gateCard.readonlyAssignments": "分工 · 只读查看",
    "gateCard.autoDispatch": "自动派",
    "gateCard.manualDispatch": "手动派",
    "gateCard.unassigned": "未派",
    "gateCard.manualIntro": "你来填目标 + 验收，填好就开跑。",
    "gateCard.autoIntro":
      "读完需求，我这么拆 —— 目标 + {count} 条验收 + 派活。你过一眼，要改哪儿点着改，行了就开跑。",
    "gateCard.draft": "草案",
    "gateCard.headerTitle": "本轮目标 · 审批即派出成员",
    "gateCard.tierNote": "过一眼 · 要改哪儿点着改 · 行了就开跑。",
    "gateCard.goalLabel": "目标（Lead 复述·你确认理解对了）",
    "gateCard.editGoalAria": "编辑目标",
    "gateCard.emptyGoal": "（待填）",
    "gateCard.edit": "改",
    "gateCard.acceptanceTitle": "验收标准",
    "gateCard.acceptanceHint": "· 你最该看这里：什么算做完，由你定",
    "gateCard.criterionAria": "验收 {index}",
    "gateCard.criterionPlaceholder": "写一条验收…",
    "gateCard.deleteCriterionAria": "删除这条验收",
    "gateCard.showRemaining": "展开剩余 {count} 条",
    "gateCard.addCriterion": "+ 加一条验收",
    "gateCard.assignments": "分工",
    "gateCard.assignmentHint": " · Lead 按能力派（点开调）",
    "gateCard.freezing": "开跑中…",
    "gateCard.confirmAndStart": "确认并开跑",
    "gateCard.start": "开始执行",
    "gateCard.redraft": "让 Lead 重拟",
    "gateCard.readonlyCannotStart": "只读模式下不能开跑",
    "gateCard.freezeHint": "点击即冻结计划并派出成员",
    "approvalCard.approvedCriterion": "已采纳",
    "approvalCard.approvedCommand": "已放行",
    "approvalCard.approvedCriterionNote": "你采纳了该验收提议",
    "approvalCard.approvedCommandNote": "你放行了此命令 · 执行中",
    "approvalCard.rejectedCriterion": "已否决",
    "approvalCard.rejectedCommand": "已拒绝",
    "approvalCard.rejectedCriterionNote": "你否决了该验收提议 · 已回告 agent",
    "approvalCard.rejectedCommandNote": "你拒绝了此命令 · 工具失败已回喂 agent",
    "approvalCard.cancelled": "已取消",
    "approvalCard.cancelledNote": "会话已结束 · 审批取消",
    "approvalCard.pendingCriterionTitle": "提议验收标准",
    "approvalCard.pendingCommandTitle": "需要放行",
    "approvalCard.pendingLabel": "等待决定",
    "approvalCard.criterionProposal": "验收提议",
    "approvalCard.criterionLabel": "验收",
    "approvalCard.commandLabel": "命令",
    "approvalCard.directory": "目录",
    "approvalCard.criterionHint":
      "采纳后该验收标准加入本轮目标；否决则该提议作废、agent 继续。",
    "approvalCard.commandHint":
      "放行后该命令在工作区内执行；拒绝则该工具失败、agent 继续别的路子。",
    "approvalCard.denyCriterion": "否决",
    "approvalCard.denyCommand": "拒绝",
    "approvalCard.allowCriterion": "采纳",
    "approvalCard.allowCommand": "放行",
    "assignmentEditor.title": "分工 · Lead 按能力派了起点，你可改",
    "assignmentEditor.autoDispatch": "自动派",
    "assignmentEditor.reassignAria": "改派 / 换模型",
    "assignmentEditor.unassigned": "未派",
    "assignmentEditor.reassignTo": "改派给",
    "assignmentEditor.availabilityNote":
      "只列已启用且当前可用的 agent · 未启用 / 不存在的派单挡下 · 更细的仓库 / 命名空间约束后续再接",
    "assignmentEditor.removeMember": "移除该成员",
    "assignmentEditor.addTask": "+ 加一块活儿",
    "assignmentEditor.leadValidationNote":
      "本地校验 / 复验 = Lead（Claude）自己做",
    "agentDropdown.selectAria": "选择 agent",
    "agentDropdown.title": "选 agent（单选）",
    "agentDropdown.empty": "暂无可用 agent · 去 Settings 配置",
    "agentDropdown.manage": "管理 agent →",
    "modeDropdown.select": "选择模式：{label}",
    "modeDropdown.current": "当前",
    "modeDropdown.normal.label": "Normal · 单 agent",
    "modeDropdown.normal.description": "你和一个选对的搭档，专注当前这件事",
    "modeDropdown.collaboration": "多 agent 协作",
    "modeDropdown.team.description": "Lead 带成员 · 自动派活",
    "modeDropdown.round.description": "主持带平级脑暴",
    "modeDropdown.soonTitle": "{label}（即将）",
    "modeDropdown.soon": "即将",
    "leadSummary.status.failed": "未完成 · {succeeded}/{total}",
    "leadSummary.status.partial": "部分完成 · {succeeded}/{total}",
    "leadSummary.advice.rateLimit":
      "建议：换一个有额度的模型重派，或稍后再试。",
    "leadSummary.advice.default":
      "建议：换一个可用 worker 重派；如果你要自己接着处理，直接切回 Normal。",
    "leadSummary.failure.withReason": "worker 调用失败：{reason}",
    "leadSummary.failure.stalled": "worker 停摆：{reason}",
    "leadSummary.failure.budgetExhausted": "worker 预算耗尽：{reason}",
    "leadSummary.failure.contextExhausted": "worker 上下文耗尽：{reason}",
    "leadSummary.failure.noResult": "worker 调用失败：未返回可用结果",
    "memberFailure.reason.quota": "API 额度/频控限制",
    "memberFailure.reason.localCodexMcpAuth": "本地 Codex/MCP 鉴权失败",
    "memberFailure.reason.auth": "API 鉴权失败",
    "memberFailure.reason.overload": "API 服务繁忙/过载",
    "memberFailure.reason.stalled": "工人停摆：等回答或被阻塞，不是环境故障",
    "memberFailure.reason.budgetExhausted":
      "工人轮次预算耗尽：任务还在正常推进，不是卡住",
    "memberFailure.reason.contextExhausted":
      "工人上下文窗口装不下了：单轮 token 预算耗尽，不是卡住",
    "memberFailure.reason.env": "worker 进程/环境故障",
    "memberFailure.reason.spawn": "worker 调用失败",
    "memberFailure.reason.noFinalText": "worker 未返回结果",
    "memberFailure.code.blockedQuestions": "有问题在等回答，已停下",
    "leadSummary.workerFailure.trace": "worker 调用失败：{reason}（见 trace）",
    "leadSummary.workerFailure.stalledTrace":
      "worker 停摆：{reason}（见 trace）",
    "leadSummary.workerFailure.budgetExhaustedTrace":
      "worker 预算耗尽：{reason}（见 trace）",
    "leadSummary.workerFailure.contextExhaustedTrace":
      "worker 上下文耗尽：{reason}（见 trace）",
    "leadSummary.workerFailure.noResultTrace": "worker 未返回结果（见 trace）",
    "leadSummary.workerFailure.emptyPassthroughTrace":
      "（worker 未产出文本·见 trace）",
    "leadSummary.workerFailure.emptyFallbackTrace": "（无文本·见 trace）",
    "leadSummary.section.changes": "改动",
    "leadSummary.section.verify": "验证",
    "leadSummary.section.risk": "风险",
    "leadSummary.section.fallback": "{name}（综合失败·原文透传）",
    "leadSummary.section.changes.table":
      "| 文件 | 改了什么 | 变更 |\n| --- | --- | --- |\n{rows}",
    "leadSummary.section.verify.command": "- `{cmd}`（退出码 {code}）",
    "leadSummary.coding.applied": "改动已落地到当前分支。",
    "leadSummary.coding.landingBlocked":
      "改动未自动落地：缺少可靠验证或安全闸未通过。请先在 Review 里查看改动，确认后再继续。",
    "leadSummary.coding.shelved": "改动已搁置（未落地）。",
    "leadSummary.coding.verify.verdict": "- `{cmd}`（{verdict}）",
    "leadSummary.coding.verify.executed": "- `{cmd}`（已执行）",
    "leadSummary.finding.failure": "{name}：{reason}",
    "leadSummary.trust.insufficientEvidence": "待复验 · 证据不足",
    "leadSummary.trust.commandTrace": "命令留痕",
    "leadSummary.trust.workerReport": "worker 自报",
    "leadSummary.trust.waived": "已跳过",
    "leadSummary.trust.unverified": "待复验",
    "leadSummary.pending": "lead 正在综合各成员产出…",
    "leadSummary.stopped.status": "已停止",
    "leadSummary.stopped.message":
      "已停下这个 worker。你可以直接告诉我接下来怎么改。",
    "leadSummary.findings.done": "已完成",
    "leadSummary.findings.miss": "没做到",
    "leadAsk.rationale": "为什么：{rationale}",
    "decisionCard.recommended": "推荐",
    "decisionCard.hint": "选择一项即回复",
    "decisionCard.questionExpand": "展开问题 ▾",
    "decisionCard.questionCollapse": "收起 ▴",
    "decisionCard.chosen": "已选：{option}",
    "decisionCard.rationaleToggle": "为什么先问 {indicator}",
    "decisionCard.retry": "重试",
    "draftFailed.parseExhausted":
      "Lead 连试 {attempts} 次都没吐出能用的结构化拆解（{lastError}）。",
    "draftFailed.invokeFailed": "调 Lead 拟计划时出错：{reason}。",
    "draftFailed.title": "Lead 拟失败",
    "draftFailed.retry": "重试拟",
    "draftFailed.manual": "手动填 gate",
    "draftFailed.backToNormal": "退回 Normal",
    "sidebar.newSessionDisabledTitle":
      "请先添加 repo · 0-repo namespace 无法建 session",
    "sidebar.newSessionTitle": "新建会话",
    "sidebar.collapse": "折叠会话栏",
    "sidebar.back": "后退",
    "sidebar.forward": "前进",
    "sidebar.overview": "总览",
    "sidebar.overviewTitle": "总览首页",
    "sidebar.search": "搜索",
    "sidebar.searchTitle": "搜索 / 命令 ⌘K（即将）",
    "sidebar.projectIntro": "项目简介",
    "sidebar.sessions": "会话",
    "sidebar.newSession": "＋ 新会话",
    "sidebar.groupNamePlaceholder": "分组名称…",
    "sidebar.newGroup": "＋ 新建分组",
    "sidebar.archived": "已归档 ({n})",
    "sidebar.resize": "拖拽调整会话栏宽度",
    "sessionRow.pinned": "已置顶",
    "sessionRow.saveShort": "存",
    "sessionRow.cancelShort": "消",
    "sessionRow.unread": "未读",
    "sessionRow.rename": "重命名",
    "sessionRow.more": "更多",
    "sessionGroup.rename": "重命名",
    "sessionGroup.delete": "删除分组",
    "sessionMenu.back": "‹ 返回",
    "sessionMenu.ungrouped": "未分组",
    "sessionMenu.newGroup": "＋ 新建分组…",
    "sessionMenu.groupNamePlaceholder": "分组名称…",
    "sessionMenu.unpin": "取消置顶",
    "sessionMenu.pin": "置顶",
    "sessionMenu.markRead": "标记已读",
    "sessionMenu.markUnread": "标记未读",
    "sessionMenu.rename": "重命名",
    "sessionMenu.moveContinuationGroup": "移动接续会话组 ▸",
    "sessionMenu.moveToGroup": "移到分组 ▸",
    "sessionMenu.restoreContinuationGroup": "恢复接续会话组",
    "sessionMenu.restore": "恢复",
    "sessionMenu.archiveContinuationGroup": "归档接续会话组",
    "sessionMenu.archive": "归档",
    "sessionMenu.stopBeforeDelete": "请先停止运行再删除",
    "sessionMenu.delete": "删除",
    "continuation.panel.label": "交接草稿",
    "continuation.panel.headerTitle": "交接草稿",
    "continuation.panel.generated": "自动生成",
    "continuation.panel.editable": "可编辑",
    "continuation.panel.parent": "父会话",
    "continuation.panel.turns": "{n} 轮",
    "continuation.panel.cancel": "取消",
    "continuation.panel.start": "启动子会话",
    "continuation.panel.starting": "启动中…",
    "continuation.panel.retry": "重新总结",
    "continuation.panel.editToggle": "编辑",
    "continuation.panel.doneEditing": "完成编辑",
    "continuation.panel.startDisabledHint":
      "请先填写「目标」和「下一步」才能启动",
    "continuation.panel.v3.loading": "正在生成交接文档…",
    "continuation.panel.v3.suggestedTitleLabel": "建议会话名",
    "continuation.panel.v3.editToggle": "编辑",
    "continuation.panel.v3.doneEditing": "完成编辑",
    "continuation.panel.v3.warningsLabel": "注意事项",
    "continuation.panel.v3.errorBackend": "后端：",
    "continuation.panel.v3.errorKey": "密钥类：",
    "continuation.panel.v3.errorParser": "解析类：",
    "continuation.panel.v3.errorBusy": "上一次生成还在进行中，请稍候重试",
    "continuation.panel.v3.loadingSub": "读取会话历史并总结，可能需要几十秒",
    "continuation.panel.v3.retry": "重试",
    "continuation.panel.v3.startDisabledHint": "交接文档不能为空",
    "continuation.panel.v3.readOnly": "只读",
    "continuation.menu.handover": "交接到新会话",
    "continuation.menu.disabled.archived": "归档会话不能交接",
    "continuation.menu.disabled.running": "请先停止运行再交接",
    "continuation.menu.disabled.continued": "会话已交接",
    "continuation.menu.disabled.assembling": "正在生成交接文档",
    "continuation.notice.ready": "交接草稿已就绪：{title}",
    "continuation.lineage.parentBadge": "已交接到 →",
    "continuation.lineage.childBadge": "↳ 接续自 {title}",
    "continuation.lineage.childTooltip": "接续自 {title}",
    "continuation.lineage.fallbackParent": "父会话",
    "topbar.tasks.view": "查看后台任务",
    "topbar.tasks.count": "后台任务 · {n} 个",
    "surfaceHeader.expandSidebar": "展开会话栏",
    "surfaceHeader.back": "后退",
    "surfaceHeader.forward": "前进",
    "surfaceHeader.overview": "总览",
    "surfaceHeader.overviewTitle": "总览首页",
    "goalBar.done": "完成",
    "goalBar.label": "本轮目标",
    "goalBar.criteriaCount": "目标 {total} 条",
    "goalBar.pendingReview": "运行已完成 · {count} 条验收待复核",
    "goalBar.viewCriteria": "查看验收",
    "goalCriteriaPanel.goal": "本轮目标",
    "goalCriteriaPanel.criteria": "验收标准",
    "goalCriteriaPanel.empty": "本轮暂无验收标准",
    "sessionMain.finished": "已收工 ✓",
    "sessionContextBar.menu": "会话菜单",
    "tasklist.stop": "停止 · 待接入（块②）",
    "inspector.backToList": "返回任务列表",
    "lead.crown.disabledTip": "该引擎暂不支持当队长（codex 开发中）",
    "lead.error.claudeOnly": "暂仅支持 Claude 当队长，请把队长设回 Claude",
    "app.dispatch.confirm": "确认派单",
    "app.dispatch.cancel": "取消",
    "app.interrupt.label": "上轮中断（重启）",
    "app.interrupt.redispatch": "从头干净重派一次 ›",
    "app.interrupt.dismiss": "知道了",
    "app.coding.appliedWithHead": "已落地到当前分支 · {head}",
    "app.coding.applied": "已落地到当前分支",
    "app.coding.awaitingDelivery": "改动在隔离区 · 待你决定要不要提交",
    "app.coding.landingBlocked": "落地前安全检查未通过",
    "app.coding.error": "出错",
    "backend.ui.badLocale": "无效的界面语言",
    "backend.agent.missingApiKey": "缺少 API key",
    "backend.agent.unknownAccess": "未知 agent access：{access}",
    "backend.agent.notFound": "未知 agent",
    "backend.agent.missingId": "缺少 agent_id",
    "backend.agent.invalidReasoningTier": "无效 reasoning_tier：{tier}",
    "backend.agent.nativeAccessImmutable": "原生 agent 不可切换接入方式",
    "backend.agent.nativeKeyUnsupported": "原生 agent 不可设置 key",
    "backend.agent.keychainSaveFailed":
      "无法将 API key 保存到系统钥匙串，key 未生效。请重试或检查系统钥匙串权限。（详情：{detail}）",
    "backend.agent.keychainKeyUnavailable": "{detail}",
    "backend.agent.sessionRunUnknown":
      "resolve_session_run_agent: 会话尚无运行记录·无法判定 agent",
    "backend.agent.idNotFound": "agent {id} 不存在",
    "backend.agent.emptyFilteredId": "agent id 过滤后为空",
    "backend.agent.unknownEngine": "未知引擎：{engine}",
    "backend.agent.configDirCreateFailed": "创建配置目录失败：{detail}",
    "backend.agent.missingEndpoint": "agent {id} 缺少 endpoint",
    "backend.member.notInSessionPool": "agent {id} 不在当前会话成员池",
    "backend.member.unavailableMissing": "agent {id} 不可用：不存在",
    "backend.member.unavailableDisabled": "agent {id} 不可用：disabled",
    "backend.member.emptyTeam": "team run 至少需要一个成员",
    "backend.member.spawnFailed": "成员启动失败：{detail}",
    "backend.member.noResult": "run_single_worker：worker 未产生 MemberResult",
    "backend.gh.gitSpawnFailed": "git 启动失败：{detail}",
    "backend.mcp.noPort": "无法取得监听端口",
    "backend.proxy.noPort": "代理无法解析监听端口",
    "backend.criteria.lineTooLong": "验收标准单行过长（>{max}）",
    "backend.criteria.invalidSyntax":
      "验收标准语法非法：{raw}（用 cmd:/contains:<s>:/judge:）",
    "backend.criteria.tooMany": "验收标准过多（>{max}）",
    "backend.file.markdownOnly": "仅允许 .md/.markdown",
    "backend.file.parentMissing": "父目录不存在",
    "backend.file.pathOutOfBounds": "路径越界",
    "backend.file.notFound": "文件不存在",
    "backend.file.openFilesOnly": "只能打开文件",
    "backend.file.tooLarge":
      "文件过大：{size} bytes，当前只预览 {max} bytes 以内的文本文件",
    "backend.file.binaryPreviewUnsupported": "暂不支持二进制文件预览",
    "backend.file.htmlOnly": "只能在浏览器打开 .html/.htm 文件",
    "backend.file.ambiguousBasename":
      "同名文件有多个，请用更完整的路径：{0} → {1}",
    "backend.file.basenameBudget":
      "同名文件太多，搜索范围超限，请提供更完整的路径（{0}）",
    "backend.file.openExternalFailed": "无法在系统浏览器打开文件：{detail}",
    "backend.file.repoLookupFailed": "查项目失败：{detail}",
    "backend.file.repoNotFound": "项目不存在",
    "backend.repo.namespaceLookupFailed": "查 namespace 失败：{detail}",
    "backend.repo.lookupFailed": "查 repo 失败：{detail}",
    "backend.repo.activeReposLookupFailed": "查 active repos 失败：{detail}",
    "backend.repo.duplicateLookupFailed": "查重失败：{detail}",
    "backend.repo.setLastActiveFailed": "set last_active 失败：{detail}",
    "backend.repo.ensureNamespaceFailed": "ensure namespace 失败：{detail}",
    "backend.repo.insertRepoFailed": "插入 repo 失败：{detail}",
    "backend.repo.namespaceMismatch":
      "REPO_NAMESPACE_MISMATCH:repo {repoId} 属 {actualNamespaceId} 非 {namespaceId}",
    "backend.repo.pathNotFound": "路径不存在：{path}",
    "backend.repo.pathNotDirectory": "路径不是目录：{path}",
    "backend.repo.pathInsideAppDomain":
      "不能添加 AgentLoom 自己的数据目录（~/.agentloom）中的项目：{path}。请把项目移到该目录之外后重试。",
    "backend.repo.insertFailed": "插入失败：{detail}",
    "backend.landing.protectedPath": "落地前检查未通过：受保护路径 {paths}",
    "backend.landing.noEvidence":
      "落地前检查未通过：找不到 worker changed_files 证据",
    "backend.landing.scopeExceeded":
      "落地前检查未通过：改动超出 worker 声明 {files}",
    "backend.landing.l1NotGreen":
      "L1 未绿（无 passed 复验·或证据 SHA 不对应当前 commit）·不准合·见 spec L4",
    "backend.merge.stagingConflict": "改动与 staging 冲突·已拒（不自动解冲突）",
    "backend.finalize.noChanges": "worker 没有改动·无可固化",
    "backend.finalize.gitUnavailable":
      "当前项目不是 git 仓库；agent 改动已保留，但 git 接力不可用",
    "backend.finalize.uncommittedChanges":
      "worker 留有未提交改动；app 不会自动提交，改动已保留在工作目录",
    "backend.artifact.notReadyVerify":
      "artifact 未 ready（无 commit_sha）·不能验",
    "backend.artifact.noShaPreflight": "artifact 无 commit_sha·不能做落地检查",
    "backend.artifact.notReadyMerge":
      "artifact 未 ready（state={state}）·不能合",
    "backend.artifact.noShaMerge": "artifact 无 commit_sha·不能合",
    "backend.artifact.notFound": "artifact 不存在：{id}",
    "backend.run.repoNotFound": "repo {id} 不存在",
    "backend.run.preHeadReadFailed": "读 pre_head 失败：{detail}",
    "backend.run.ledgerPendingWriteFailed": "写 ledger pending 失败：{detail}",
    "backend.run.spawnFailed": "启动失败：{detail}",
    "backend.run.teamMembersActive": "无法开始新运行：{detail}",
    "backend.run.stdoutUnavailable": "无法读取输出",
    "backend.run.workspaceCanonicalizeFailed": "canonicalize 失败：{detail}",
    "backend.run.unknownLeadAgent": "未知 lead agent：{id}",
    "backend.run.unknownLeadAgentGeneric": "未知 lead agent",
    "backend.run.tombstoneRestoreFailed":
      "TOMBSTONE_FAILED_RESTORE_FAILED:tombstone={tombstone};restore={restore}（DB/git 背离·需刀二b reconcile）",
    "backend.run.invalidSessionId": "session_id 无效",
    "backend.run.inplaceDeliveryUncommitted":
      "就地改动尚未提交：{count} 个文件仍在工作区（{files}）。请先在项目里提交这些改动，再推送 / 创建 PR / 发布。",
    "backend.delivery.confirmationRequired":
      "{operation} 需要本次用户明确确认，未执行任何远端操作",
    "backend.publish.pushed": "已推送到 origin/{branch}",
    "backend.publish.failed.boundRepo":
      "PUBLISH_FAILED:会话已绑 GitHub repo·应走 push/PR 而非 publish",
    "backend.publish.needsAccount.missing":
      "PUBLISH_NEEDS_ACCOUNT:未检测到 gh 登录账户（gh auth login）",
    "backend.publish.needsAccount.multiple":
      "PUBLISH_NEEDS_ACCOUNT:检测到多个 gh 账户·请指定身份（{list}）",
    "backend.publish.failed": "PUBLISH_FAILED:{detail}",
    "backend.publish.failed.missingRepoName":
      "PUBLISH_FAILED:缺 repo_name（无 goal_title 可回退）",
    "backend.continuation.invalidParentSessionId":
      "parent session_id 清洗后为空，无法创建接续子会话",
    "backend.continuation.childSessionIdUnavailable":
      "无法生成唯一接续子会话 id",
    "backend.continuation.startCleanupFailed":
      "{original}; cleanup errors: {errors}",
    "backend.continuation.handoffRequired": "接续启动指令（交接文档）不得为空",
    "backend.continuation.handoffTimedOut":
      "接续草稿生成超时，请重试。父会话现已可以继续操作。",
    "backend.continuation.invalidSessionId":
      "session_id 清洗后为空，无法拼装接续交棒单",
    "backend.continuation.localSessionUnsupported":
      "本地会话暂不支持接续（此功能对本地会话尚未开放）",
    "backend.lead.claudeOnlyContinuation":
      "Team 接续暂仅支持 native claude 会话（非-claude lead 泛化中·先用 Solo）",
    "backend.apply.repoDetached":
      "当前 repo 处于 detached HEAD·不在分支上·不应用（先 checkout 一个分支）",
    "backend.apply.repoDirty":
      "当前 repo 工作树有未提交改动·先提交或暂存再应用",
    "backend.apply.branchAdvanced":
      "当前分支无法 ff 到 staging（可能已前进或分叉·v1 不强推）：当前分支已前进",
    "backend.apply.fastForwardFailed":
      "当前分支无法 ff 到 staging（可能已前进或分叉·v1 不强推）：{detail}",
    "backend.wt.verifier.unsupportedPlatform":
      "verifier sandbox 在本平台不可用·MVP 仅支持 macOS·Linux sandbox 是 follow-up",
    "backend.wt.verifier.writeAttempt":
      "verifier 试图改文件·已拒·写文件请走 dispatch_worker",
    "backend.wt.verifier.canonicalizeFailed":
      "无法规范化会话工作树/HOME 路径·verifier 拒绝在无沙箱护栏下就地跑：{detail}",
    "backend.wt.sessionMerge.artifactBaseMismatch":
      "artifact {artifact} 不基于 base_sha {base}·拒合",
    "backend.wt.sessionMerge.stagingBaseMismatch":
      "staging {staging} 不基于 base_sha {base}·拒合",
    "backend.wt.sessionMerge.outsideAppDomain":
      "Stage① 拒合：session_wt {path} 不在 app 域（~/.agentloom）·fail-closed",
    "backend.wt.sessionMerge.invalidHead":
      "Stage① 拒合：会话 wt HEAD 非 attached 到 agentloom/*（detached 或非 agentloom 分支）·fail-closed",
    "backend.wt.sessionMerge.memberMissing":
      "Stage① 拒合：member 分支 {member} 不存在",
    "backend.wt.sessionMerge.dirtyWorktree":
      "Stage① 拒合：会话 worktree 有未提交改动·fail-closed",
    "backend.wt.sessionMerge.stagingBranchMissing":
      "staging 分支不存在：{staging}",
    "backend.wt.cleanup.commitOutsideAppDomain":
      "commit_dirty 拒:{path} 不在 app 域·fail-closed",
    "backend.wt.cleanup.commitInvalidHead":
      "commit_dirty 拒:{path} HEAD 非 attached 到 agentloom/*·fail-closed",
    "backend.wt.cleanup.sessionWorktreeReleased":
      "finalize-before-cleanup 拒清:会话 wt 已释放但仍有 {pending} 个 member 分支待并·fail-closed(重建会话 wt 后重试/刀二b reconcile)",
    "backend.wt.cleanup.invalidMemberRef":
      "finalize-before-cleanup 拒清:member ref {member} 格式异常·fail-closed",
    "backend.wt.cleanup.memberWorktreeDetached":
      "finalize-before-cleanup 拒清:member worktree {path} 非 attached 到 {member}(detached/异常态)·fail-closed·交刀二b reconcile",
    "backend.wt.cleanup.notFastForward":
      "finalize-before-cleanup 拒清:member {member} 非 ff(stale-base·刀二b/并行处理)·fail-closed",
    "backend.wt.cleanup.registrationIncomplete":
      "release/trash 拒:worktree {path} 反登记未完成·fail-closed",
    "backend.wt.cleanup.trashRefExists":
      "trash 拒:{trash} 已存在·防覆盖旧 grace 副本·fail-closed(刀二b reconcile)",
    "backend.wt.restore.headsRefExists":
      "restore 拒:{heads} 已存在·防覆盖 live 分支·fail-closed(刀二b reconcile)",
    "backend.wt.restore.refsMissing":
      "restore 拒:会话 {session} 的 trash 与 heads ref 均不存在(purge 半失败/已 gc)·无可恢复·fail-closed(刀二b reconcile)",
    "backend.wt.restore.compensationTrashExists":
      "restore 补偿拒:{trash} 已存在·防覆盖 trash ref·fail-closed(刀二b reconcile)",
    "backend.wt.restore.compensationHeadsMissing":
      "restore 补偿拒:{heads} 不存在·无法移回 trash·fail-closed(刀二b reconcile)",
    "backend.wt.gc.liveWorktree":
      "gc 拒:会话 {session} 仍有活 worktree 注册·fail-closed",
    "backend.wt.gc.liveHeads":
      "gc 拒:会话 {session} 的 heads(live 分支)仍在·base 是其 diff fork 点·fail-closed(刀二b reconcile)",
    "backend.wt.session.gitStatusSpawnFailed": "git status 启动失败：{detail}",
    "backend.wt.session.gitStatusFailed": "git status 失败：{detail}",
    "backend.wt.session.worktreeDirty":
      "worktree 有未提交改动（ledger 静止态应干净）",
    "backend.wt.session.postHeadMissing":
      "ledger post_head {postHead} 在 git 中不存在",
    "backend.wt.session.postHeadNotAncestor":
      "ledger post_head {postHead} 非当前 HEAD 祖先",
    "backend.wt.session.invalidDefaultId":
      "session_id 清洗后为空，无法建默认 worktree",
    "backend.wt.session.invalidId": "session_id 清洗后为空，无法建 worktree",
    "backend.wt.session.invalidMemberIds":
      "session_id/assignment_id 清洗后为空，无法建 member worktree",
    "backend.wt.session.invalidSessionId": "session_id 无效",
    "backend.wt.continuation.invalidIds":
      "parent/child session_id 清洗后为空，无法派生接续 worktree",
    "backend.wt.continuation.childBranchExists": "接续子分支已存在：{child}",
    "backend.wt.continuation.baseRefExists": "接续子 base-ref 已存在：{base}",
    "backend.wt.continuation.invalidChildId":
      "child session_id 清洗后为空，无法清理接续 worktree",
    "backend.wt.continuation.pathNotUtf8": "worktree 路径非 UTF-8：{path}",
    "backend.wt.continuation.removeResidualFailed":
      "删除残留 worktree 目录失败：{detail}",
    "backend.wt.continuation.refsStillRegistered":
      "接续清理拒删 refs:子 worktree 仍注册：{path}",
    "backend.wt.git.spawnFailed": "git 启动失败：{detail}",
    "backend.wt.git.commandFailed": "git {cmd} 失败：{stderr}",
    "backend.wt.git.revParseSpawnFailed": "git rev-parse 启动失败：{detail}",
    "backend.wt.git.revParseFailed": "git rev-parse HEAD 失败：{stderr}",
    "backend.wt.git.sessionStatusSpawnFailed":
      "session_wt {phase}-status 失败：git 启动失败：{detail}",
    "backend.wt.git.sessionStatusFailed":
      "session_wt {phase}-status 失败：git {cmd} 失败：{stderr}",
    "backend.wt.git.verifierSpawnFailed": "验证命令启动失败：{detail}",
    "backend.wt.git.worktreeListFailed": "git worktree list 失败：{detail}",
    "backend.wt.git.worktreeListNonZero":
      "git worktree list 退出非 0(code {exitCode})：{stderr}",
    "backend.wt.scaffold.worktreeAddSpawnFailed":
      "git worktree add 启动失败：{detail}",
    "backend.wt.scaffold.verifyCheckoutFailed":
      "建临时 verify checkout 失败：{stderr}",
    "backend.wt.scaffold.stagingWorktreeFailed":
      "建 staging worktree 失败：{stderr}",
    "backend.wt.scaffold.createDirFailed": "建目录失败：{detail}",
    "backend.wt.scaffold.defaultInitSpawnFailed": "git init 启动失败：{detail}",
    "backend.wt.scaffold.defaultInitFailed": "git init 失败：{stderr}",
    "backend.wt.scaffold.sessionWorktreeSpawnFailed":
      "git worktree 启动失败：{detail}",
    "backend.wt.scaffold.sessionWorktreeFailed": "git worktree 失败：{stderr}",
    "backend.wt.scaffold.continuationWorktreeSpawnFailed":
      "git continuation worktree 启动失败：{detail}",
    "backend.wt.scaffold.continuationWorktreeFailed":
      "git continuation worktree 失败：{stderr}",
    "backend.wt.scaffold.memberWorktreeSpawnFailed":
      "git member worktree 启动失败：{detail}",
    "backend.wt.scaffold.memberWorktreeFailed":
      "git member worktree 失败：{stderr}",
    "backend.db.restore.parentMissing": "父会话不存在，无法恢复接续子会话",
    "backend.db.restore.parentDeleted": "父会话已删除，无法恢复接续子会话",
    "backend.db.restore.parentPointsElsewhere":
      "父会话 continued_to_session_id 指向其它子会话，无法恢复接续子会话",
    "backend.db.restore.liveChildExists":
      "父会话已有 live 子会话，无法恢复旧接续子会话",
    "backend.db.memory.badJson": "{field} 非合法 JSON: {detail}",
    "backend.lead.spawnDriverFailed": "spawn driver 失败：{detail}",
    "backend.lead.spawnLeadFailed": "spawn lead 失败：{detail}",
    "backend.lead.noFinalText": "lead 无终态 final_text",
    "backend.lead.noFinalTextStderr":
      "lead 无终态 final_text·stderr 尾部：{stderr}",
    "backend.lead.claudeOnlyBlock1":
      "块① 仅支持 native claude 队长（当前 provider={provider} access={access}）",
    "backend.lead.engineNotSupported":
      "当前引擎暂不支持当队长（provider={provider} access={access}）",
    "backend.team.oneshotSpawnFailed": "run_oneshot_llm 启动失败：{detail}",
    "backend.team.oneshotFailed": "run_oneshot_llm 失败：{detail}",
    "backend.team.oneshotNoText": "run_oneshot_llm 没有产生 assistant 文本",
    "backend.team.noMemberOutput": "无队员产出·无法综合",
    "backend.team.summarizeSpawnFailed": "lead_summarize 启动失败：{detail}",
    "backend.team.summarizeFailed": "lead 综合失败：{detail}",
    "backend.team.summarizeNoText": "lead 综合没有产生 assistant 文本",
    "backend.lead.claudeOnlyStep":
      "lead_step 仅支持 native claude lead（当前 access={access} provider={provider}）",
    "backend.lead.parseSpawnFailed": "lead 输出无法解析：{detail}",
    "backend.lead.parseNoOutput": "lead 输出无法解析：{detail}",
    "backend.lead.parseFailed": "lead 输出无法解析：{detail}",
    "backend.lead.claudeOnlyDraft":
      "B1 仅支持 native claude 作为 driver（当前 access={access} provider={provider}·borrow/codex driver 留 follow-up）",
    "backend.lead.draftNoFinalText": "driver 无终态 final_text 或报错",
    "backend.lead.draftNoFinalTextStderr":
      "driver 无终态 final_text 或报错·stderr 尾部：{tail}",
    "backend.leadTools.askUserNeedsOptions": "ask_user 需 options (至少 2 个)",
    "backend.leadTools.verifierLocalUnsupported":
      "propose_verifier: Local 会话暂不支持",
    "app.coding.awaitingDeliveryNarration":
      "干完了，改动在隔离区，等你决定要不要提交 / 推 / 开 PR（看下方改动条）",
    "app.session.new": "新会话",
    "app.dispatch.noAvailableMembers": "当前 Team 没有可派成员。",
    "app.dispatch.hintNotFound":
      "agent_hint「{hint}」不在可调度成员池里·不派单。",
    "app.dispatch.hintAmbiguous":
      "agent_hint「{hint}」命中多个可调度成员·不派单。",
    "app.dispatch.hintRequired":
      "可调度成员超过 1 个但 lead 未给 agent_hint·不派单（须由 lead 指定一个 worker）。",
    "app.dispatch.scopeSuggestion": "{task}\n（建议改动范围：{scope}）",
    "app.listSeparator": "、",
    "app.dispatch.question": "派给 {name}：{task}",
    "app.verify.noActiveChanges":
      "我想用「{command}」验证一下，但当前没有进行中的改动。要我先派 worker 做出来再验吗？",
    "app.verify.dispatchFirst": "先派 worker",
    "app.verify.later": "先放着",
    "app.delivery.noChanges": "{rationale} · 没有待交付的改动",
    "app.delivery.applied": "{rationale} · 已落地到当前分支（{head}）",
    "app.delivery.prCreated": "{rationale} · 已开 PR：{url}",
    "app.delivery.published": "{rationale} · 已发布：{url}",
    "app.delivery.confirmPush":
      "确认把当前分支推送到远端？此操作无法由撤销恢复。",
    "app.delivery.confirmCreatePr":
      "确认推送当前分支并创建 Pull Request？这些远端操作无法由撤销恢复。",
    "app.delivery.confirmPublish":
      "确认创建远端仓库并发布当前项目？此操作无法由撤销恢复。",
    "app.lead.transientQuestion":
      "队长这次没接上（可能临时不可用），换个说法再说一下？",
    "app.lead.genericQuestion": "队长没想清楚下一步，换个说法再讲一遍？",
    "app.lead.retry": "我重说一句",
    "app.lead.transientRationale": "lead 暂时没回应",
    "app.lead.genericRationale": "lead_step 失败",
    "app.run.error": "\n[出错] {message}",
    "app.run.stopped": "\n[已停下] {message}",
    "app.repo.alreadyAdded": "已在列表 · 已切到该项目",
    "app.session.moveRepoMismatch": "该会话不属于此 repo，无法移入",
    "app.run.startFailed": "[启动失败] {error}",
    "app.session.deleteBody": "会话将从列表移除，保留 30 天后永久清除。",
    "app.session.deleteBodyWithContinuations":
      "此会话还有 {count} 个活跃的接续会话。删除只会删除当前会话，接续会话会保留为独立会话。当前会话将从列表移除，保留 30 天后永久清除。",
    "app.gate.goalRequired": "请先填写目标再冻结。",
    "app.gate.unassigned":
      "还有子任务没派到 agent·点分工里的「未派 ▾」选一个再开跑。",
    "app.header.sessionFallback": "会话",
    "app.header.overviewContext": "总览 · {name}",
    "app.header.introContext": "{name} · 项目简介",
    "app.project.archived": "项目已归档",
    "app.project.restored": "项目已恢复 active",
    "app.project.switchedDefault": "已切到默认会话",
    "app.session.deleteTitle": "删除会话「{title}」？",
    "app.dialog.delete": "删除",
    "app.group.deleteTitle": "删除分组「{name}」？",
    "app.group.deleteBody": "组内会话会回到未分组（不删会话）。",
    "repoSwitcher.searchPlaceholder": "搜索项目或 owner…",
    "repoSwitcher.empty": "还没有项目 · 用下方入口新建或连接",
    "repoSwitcher.projectsGroup": "项目",
    "repoSwitcher.newProject": "新建项目",
    "repoSwitcher.editProject": "编辑项目",
    "repoSwitcher.manageRepos": "管理 GitHub 仓库",
    "newProject.title": "新建项目",
    "newProject.editTitle": "编辑项目",
    "newProject.hint": "项目是会话的家——把一件事的所有对话和文件放在一起。",
    "newProject.name.label": "名称",
    "newProject.name.placeholder": "项目名称",
    "newProject.location.label": "位置",
    "newProject.location.newFolder": "新建文件夹",
    "newProject.location.existingFolder": "选择已有文件夹",
    "newProject.location.existingHint":
      "把现有文件夹变成项目（不移动、不改动其中文件）",
    "newProject.location.willCreate": "将创建 {path}",
    "newProject.identity.label": "标识（emoji）",
    "newProject.identity.option": "项目标识 {emoji}",
    "newProject.cta.cancel": "取消",
    "newProject.cta.create": "创建项目",
    "newProject.cta.save": "保存",
    "newProject.cta.remove": "移除项目",
    "removeProject.confirm.title": "移除项目？",
    "removeProject.confirm.body":
      "移除「{name}」？它的 {count} 个会话会一起隐藏（数据保留·磁盘代码不动），可在 设置 › 已归档项目 恢复。",
    "removeProject.confirm.confirm": "移除",
    "removeProject.confirm.cancel": "取消",
    "namespaceDropdown.searchPlaceholder": "搜索 namespace…",
    "namespaceDropdown.builtin": "内置",
    "namespaceDropdown.cannotDelete": "不可删",
    "namespaceDropdown.manageRepos": "管理仓库",
    "namespaceDropdown.connectGithub": "连接 GitHub repo",
    "repoDropdown.searchPlaceholder": "搜索 repo…",
    "projectSwitcher.selectProject": "选择项目",
    "projectSwitcher.myProject": "我的项目",
    "projectSwitcher.ariaLabel": "项目切换器",
    "projectSwitcher.settings": "设置",
    "projectSwitcher.sessionCount": "{n} 会话",
    "invalidProjectDialog.title.invalid": "项目路径已无效",
    "invalidProjectDialog.title.archived": "项目已归档",
    "invalidProjectDialog.body.invalid":
      "项目原路径找不到了（可能已移动或删除）。你可以归档此项目（保留历史会话）或切到默认会话继续工作。",
    "invalidProjectDialog.body.archived":
      "项目已归档不参与日常使用。你可以恢复成 active 项目，或切到默认会话继续工作。",
    "invalidProjectDialog.switchDefault": "跳到默认会话",
    "invalidProjectDialog.archive": "归档此项目",
    "invalidProjectDialog.restore": "恢复",
    "confirmDialog.cancel": "取消",
    "cloneProgress.openSession": "打开会话",
    "cloneProgress.cloning": "克隆中…",
    "cloneProgress.occupied": "位置被占用",
    "cloneProgress.retry": "重试",
    "cloneProgress.nonBlocking":
      "非阻塞：克隆完的可直接开会话，不必等其余；失败的就地重试，不影响其它仓库。",
    "scrollButtons.top": "回到顶部",
    "scrollButtons.bottom": "回到底部",
    "repoConnection.error.notGit": "这不是 git 仓库。",
    "repoConnection.error.notGithub":
      "这不是 GitHub repo（要关联本地项目请用 Local）。",
    "repoConnection.error.noCommits": "这个 repo 还没有任何 commit，暂不支持。",
    "repoConnection.error.alreadyAdded":
      "这个 repo 已经添加过了，已为你切过去。",
    "repoConnection.error.generic": "连接失败，请重试。",
    "repoManage.error.offline":
      "检测不到网络。本地会话与已克隆仓库仍可用；GitHub 发现/克隆暂不可用。",
    "repoManage.error.timeout": "读取 GitHub 仓库超时，请检查网络后重试。",
    "repoManage.error.accountTimeout":
      "读取 GitHub 账户超时，请检查系统凭据或稍后重试。",
    "repoManage.error.authExpired": "@{login} 授权失效，请重新登录",
    "repoManage.error.loadFailed": "仓库列表加载失败",
    "repoManage.gate.missing.title": "需要 GitHub CLI (gh)",
    "repoManage.gate.missing.description": "列远端仓库与克隆需要 gh。",
    "repoManage.gate.checking.title": "正在检查仓库环境…",
    "repoManage.gate.checking.description": "确认 Git、GitHub CLI 与账户状态。",
    "repoManage.gate.missingGit.title": "需要 Git",
    "repoManage.gate.missingGit.description":
      "AgentLoom 使用 Git 管理工作区和改动。",
    "repoManage.gate.missingGit.install": "安装 Git",
    "repoManage.gate.accountError.title": "GitHub 账户读取失败",
    "repoManage.gate.installing": "安装中…",
    "repoManage.gate.install": "一键安装 gh",
    "repoManage.gate.manualInstall": "手动安装文档",
    "repoManage.gate.connect.title": "连接 GitHub 账户",
    "repoManage.gate.connect.instructions.prefix": "在终端运行",
    "repoManage.gate.connect.instructions.suffix": "，完成后点刷新。",
    "repoManage.refresh": "刷新",
    "repoManage.recheck": "重新检测",
    "repoManage.title": "仓库 · 管理",
    "repoManage.subtitle": "按账户发现 → 按需克隆",
    "repoManage.switchAccount": "切换账户",
    "repoManage.checkingAria": "正在检查仓库环境",
    "repoManage.checking": "正在检查环境…",
    "repoManage.status.missingGit": "Git 未安装",
    "repoManage.status.missingGh": "gh 未安装",
    "repoManage.status.accountError": "账户读取失败",
    "repoManage.status.noAccount": "未连接 GitHub 账户",
    "repoManage.loadingAria": "正在读取仓库",
    "repoManage.loading": "正在读取仓库…",
    "repoManage.idle": "尚未读取远端仓库",
    "repoManage.read": "读取",
    "repoManage.counts": "{cloned} 已克隆 / {remote} 远程",
    "repoManage.retry": "重试",
    "repoManage.selection.summaryPrefix": "已选",
    "repoManage.selection.summarySuffix": "个 · 存放到",
    "repoManage.selection.destinationHint":
      "（自动按 github.com/org/repo 归类）",
    "repoManage.selection.identity": "以 @{login} 身份提交",
    "repoManage.clone": "克隆",
    "repoManage.connectLocal": "添加本地已克隆的仓库",
    "repoManage.connectAction": "连接 GitHub 账号",
    "repoManage.connect.instructions.prefix": "在终端运行",
    "repoManage.connect.instructions.suffix": " 添加账户，完成后点刷新。",
    "repoList.status.occupied": "位置被占用",
    "dispatchCard.goalFallback": "本轮任务",
    "teamRun.errorPrefix": "错误：",
    "repoList.search.placeholder": "搜索仓库…",
    "repoList.filter.all": "全部",
    "repoList.filter.cloned.first": "已",
    "repoList.filter.cloned.second": "克隆",
    "repoList.filter.remote.first": "远",
    "repoList.filter.remote.second": "程",
    "repoList.group.batch": "克隆中 · 本批次",
    "repoList.status.cloningAria": "克隆中",
    "repoList.empty": "空仓库",
    "repoList.status.cloning": "克隆中…",
    "repoList.openSession": "打开会话",
    "repoList.status.cloneFailed": "克隆失败",
    "repoList.retry": "重试",
    "repoList.group.cloned": "已克隆 · 本地可用",
    "repoList.group.remote": "远程 · 未克隆",
    "repoList.group.remoteHint": " · 勾选多个一键克隆",
    "repoList.updatedAt": "· 更新于 {value}",
  },
  en: {
    "settings.title": "Settings",
    "settings.close": "Close",
    "settings.closeSettings": "Close settings",
    "settings.version": "Version",
    "settings.nav.agents": "Agent Pool",
    "settings.nav.search": "Web Search",
    "settings.nav.repos": "Repositories",
    "settings.nav.archivedProjects": "Archived projects",
    "settings.nav.language": "Language & Region",
    "settings.nav.defaults": "Defaults & Modes",
    "settings.nav.allowlist": "Namespace Allowlist",
    "settings.nav.accounts": "Accounts & Git",
    "settings.nav.budget": "Cost & Budget",
    "settings.nav.shortcuts": "Keyboard Shortcuts",
    "settings.nav.about": "About",
    "settings.about.support": "Support",
    "settings.about.feedback": "Feedback",
    "settings.about.website": "Website",
    "aboutDialog.close": "Close",
    "aboutDialog.copyVersion": "Copy version",
    "aboutDialog.copied": "Copied",
    "aboutDialog.website": "Website",
    "aboutDialog.feedback": "Feedback",
    "aboutDialog.support": "Support",
    "aboutDialog.copyright": "© 2026 MyAgentHubs",
    "archivedProjects.empty": "No archived projects",
    "archivedProjects.restore": "Restore",
    "archivedProjects.deleteForever": "Delete forever",
    "archivedProjects.deleteConfirm.title": "Delete project forever?",
    "archivedProjects.deleteConfirm.body":
      'Permanently delete "{name}" and all its sessions (irreversible). Code on disk will not be deleted.',
    "archivedProjects.deleteConfirm.confirm": "Delete forever",
    "archivedProjects.deleteConfirm.cancel": "Cancel",
    "backend.project.cannotDeleteDefault":
      "The default project cannot be deleted.",
    "settings.search.intro":
      "Not every model ships with built-in web search. AgentLoom connects third-party search services so any agent can search the web. DuckDuckGo works out of the box — no key needed; add a Brave or Exa API key for higher-quality results.",
    "settings.search.formAriaLabel": "Search service settings",
    "settings.search.serviceLabel": "Search service",
    "settings.search.ddgNote": "DuckDuckGo needs no API key.",
    "settings.search.useThisButton": "Use this service",
    "settings.search.useThisSwitching": "Switching…",
    "settings.search.useThisSwitched": "Switched to DuckDuckGo.",
    "settings.search.useThisError": "Failed to switch, please try again",
    "settings.search.apiKeyLabel": "API Key",
    "settings.search.testButton": "Test connection",
    "settings.search.testingButton": "Testing…",
    "settings.search.saveButton": "Save",
    "settings.search.savingButton": "Saving…",
    "settings.search.saveNote":
      "Saving stores the key in the system keychain and makes this service the active one.",
    "settings.search.saved": "Saved",
    "settings.search.saveError": "Save failed, please try again",
    "settings.search.registerLink": "Register a key at {label}",
    "settings.search.placeholderConfigured":
      "Configured · paste a new key to replace it",
    "settings.search.placeholderEmpty": "Paste your {apiName} key",
    "settings.search.searxngComingSoon": "SearXNG (coming soon)",
    "settings.search.category.ok": "Connected",
    "settings.search.category.auth": "Invalid or unauthorized key",
    "settings.search.category.rateLimit": "Rate limited · try again later",
    "settings.search.category.network": "Network error",
    "settings.search.category.missingKey": "Enter a key first",
    "settings.search.status.unknown": "Not checked",
    "settings.search.status.checking": "Checking…",
    "settings.search.status.configured": "Configured",
    "settings.search.status.missing": "Missing",
    "settings.search.checkButton": "Check",
    "settings.language.title": "Language & Region",
    "settings.language.subtitle":
      "Choose the language for the AgentLoom interface.",
    "settings.language.current": "Current language",
    "settings.language.zh": "中文",
    "settings.language.en": "English",
    "settings.agentAccess.borrow": "Via Claude Code",
    "settings.agentAccess.harness": "Built-in engine",
    "settings.agentAccess.native": "Native CLI",
    "settings.agentKeyState.configured": "Configured ✓",
    "settings.agentKeyState.detected": "Detected",
    "settings.agentKeyState.missing": "Missing",
    "settings.agentKeyState.notInstalled": "Not installed",
    "settings.agents.configuredCount": "{n} agents configured",
    "settings.agents.description":
      "Add an AI worker: choose an engine, pick a model, paste a key.",
    "settings.agents.add": "＋ Add agent",
    "settings.agents.empty": "No agents yet",
    "settings.agents.listAria": "Agent pool list",
    "settings.agents.providerModel.unset": "Provider/model not set",
    "settings.agents.nativeAutoDetectTitle":
      "Uses the local CLI automatically · no setup needed",
    "settings.agents.nativeAutoDetect": "Auto-detected",
    "settings.agents.edit": "Edit",
    "settings.agents.delete": "Delete",
    "settings.agents.deleteAria": "Delete {name}",
    "onboarding.installGuide.title": "No agents are available yet",
    "onboarding.installGuide.reason":
      "AgentLoom can run agents with its built-in engine, myagent — all it needs is your own API key — or drive Claude Code or the Codex CLI on this computer. None of these is set up yet.",
    "onboarding.installGuide.harnessDescription":
      "Run agents with your own API key. No vendor CLI to install.",
    "onboarding.installGuide.configureHarness": "Set up",
    "onboarding.installGuide.claudeDescription":
      "Run Claude agents with your Anthropic account.",
    "onboarding.installGuide.codexDescription":
      "Run Codex agents with your OpenAI account.",
    "onboarding.installGuide.openInstallGuide": "Open installation guide",
    "onboarding.installGuide.openSettings": "Open Agent settings",
    "onboarding.installGuide.dismiss": "Maybe later",
    "settings.agentForm.category.auth": "Key is invalid or unauthorized",
    "settings.agentForm.category.rateLimit":
      "Quota exhausted or rate limited (not an endpoint issue)",
    "settings.agentForm.category.network":
      "Cannot reach the endpoint. Check network/address.",
    "settings.agentForm.category.notFound": "Endpoint or model not found",
    "settings.agentForm.category.missingKey": "Paste an API Key before testing",
    "settings.agentForm.category.endpointRequired": "Endpoint is required",
    "settings.agentForm.category.other": "Request failed",
    "settings.agentForm.group.account": "Account",
    "settings.agentForm.saveBlocked.nativeMissing":
      "{cli} CLI is not installed, so this cannot be saved yet",
    "settings.agentForm.saveBlocked.testFailed":
      "Connection test has not passed, so this cannot be saved yet",
    "settings.agentForm.engineStatus.builtIn": "✓ Built in · no install",
    "settings.agentForm.engineStatus.installedLoggedIn":
      "✓ Installed · signed in",
    "settings.agentForm.engineStatus.installed": "✓ Installed",
    "settings.agentForm.engineStatus.notDetected": "⚠ Not detected ·",
    "settings.agentForm.engineStatus.installGuide": "Install guide",
    "settings.agentForm.engineStatus.installGuideAria":
      "{engine} install guide",
    "settings.agentForm.nativeStatus.loggedIn":
      "✓ {cli} CLI detected · signed in with your {account} account, no API Key needed",
    "settings.agentForm.nativeStatus.installedNoCredsPrefix":
      "⚠ {cli} CLI is installed, but no login credentials were detected. You can save now; if it fails at runtime, run",
    "settings.agentForm.nativeStatus.installedNoCredsSuffix":
      "in a terminal, then",
    "settings.agentForm.nativeStatus.notDetected": "⚠ {cli} CLI not detected",
    "settings.agentForm.nativeStatus.recheck": "Recheck",
    "settings.agentForm.nativeStatus.viewInstallGuide": "View install guide",
    "settings.agentForm.moreSummary.borrow":
      "Model · reasoning · endpoint · auth · model mapping · timeout · compatibility",
    "settings.agentForm.moreSummary.harness":
      "Model · reasoning · endpoint · timeout",
    "settings.agentForm.moreSummary.native": "Model · reasoning",
    "settings.agentForm.modelLabel": "Model",
    "settings.agentForm.primaryModelLabel": "Primary model",
    "settings.agentForm.harnessModelPlaceholder": "Blank = myagent default",
    "settings.agentForm.harnessDefaultModelOption": "myagent default",
    "settings.agentForm.fromList": "↩ Choose from list",
    "settings.agentForm.unknownModelWarning":
      "Unrecognized model id — double-check the spelling (e.g. claude-fable-5). Saving is allowed, but the agent may fail to start.",
    "settings.agentForm.modelPlaceholder.cliDefault": "CLI default",
    "settings.agentForm.modelPlaceholder.select": "Select a model",
    "settings.agentForm.reasoningLabel": "reasoning default",
    "settings.agentForm.reasoningDisabledHint": "reasoning tiers are disabled",
    "settings.agentForm.authLabel": "Auth mode",
    "settings.agentForm.autoMark": "· Auto",
    "settings.agentForm.modelMappingLabel": "Model mapping",
    "settings.agentForm.modelMappingHint":
      "Important: Claude Code uses the haiku tier for background work/subagent tasks. If this endpoint does not have claude-haiku, map it to that provider's small model or background jobs may return 400/404.",
    "settings.agentForm.maxOutputTokensPlaceholder":
      "Default: follow the model limit (recommended)",
    "settings.agentForm.compatLabel":
      "Compatibility switches (Claude Code routing tweaks · usually leave unchanged)",
    "settings.agentForm.compatDisableThinking": "Disable thinking",
    "settings.agentForm.compatDisableBetas": "Disable betas",
    "settings.agentForm.compatDisableNonessential":
      "Disable nonessential traffic",
    "settings.agentForm.compatProxyPlaceholder": "e.g. thinking_passback",
    "settings.agentForm.formAria": "Add / edit agent",
    "settings.agentForm.title.add": "Add agent",
    "settings.agentForm.title.edit": "Edit agent",
    "settings.agentForm.borrowIntro":
      "Use native mode when you have the CLI (claude/codex). Other providers route through Claude Code automatically; just paste a key.",
    "settings.agentForm.basic": "Basic",
    "settings.agentForm.engineLabel": "Engine",
    "settings.agentForm.engineDesc.claudeCode":
      "Local claude command. Runs Anthropic models directly or routes other providers.",
    "settings.agentForm.engineDesc.codex":
      "Local codex command. Runs OpenAI models directly.",
    "settings.agentForm.engineDesc.myagent":
      "Custom harness that connects directly to provider APIs",
    "settings.agentForm.presetLabel.custom": "Custom",
    "settings.agentForm.providerUpcoming": "Proxy mode · later version",
    "settings.agentForm.accountChip": "{account} account",
    "settings.agentForm.cliLoggedIn": "CLI signed in",
    "settings.agentForm.accessPointLabel": "Access point",
    "settings.agentForm.accessPoint.default": "Default",
    "settings.agentForm.accessPoint.cn": "China",
    "settings.agentForm.accessPoint.intl": "International",
    "settings.agentForm.accessPoint.cn-coding": "China · Coding plan",
    "settings.agentForm.accessPoint.intl-coding": "International · Coding plan",
    "settings.agentForm.borrowPresetHint":
      "After you choose a Provider preset, model mapping / auth mode / timeout / compatibility switches are filled in automatically; providers that need a key only need an API Key.",
    "settings.agentForm.harnessHint":
      "myagent connects directly to this provider (OpenAI compatible). Pick a model (or leave it blank for the default) and paste a key; endpoint is built in, with no model mapping/auth mode needed.",
    "settings.agentForm.nameLabel": "Name",
    "settings.agentForm.apiKeyHint":
      "Stored in the local keychain · not uploaded · this machine only",
    "settings.agentForm.existingKeyPlaceholder":
      "Configured · leave blank to keep the existing key",
    "settings.agentForm.showApiKey": "Show API Key",
    "settings.agentForm.hideApiKey": "Hide API Key",
    "settings.agentForm.testing": "Testing…",
    "settings.agentForm.testConnection": "Test connection",
    "settings.agentForm.keyStatusPrefix": "Key status: ",
    "settings.agentForm.keepStoredKeyHint":
      "; leave blank to keep the stored key",
    "settings.agentForm.borrowKeyMissing":
      "No key configured · this agent is unavailable for now",
    "settings.agentForm.multiAp.keyHint":
      "{accessPoints} keys are not interchangeable · use a key from {keyHint}",
    "settings.agentForm.multiAp.noKeyHint":
      "{accessPoints} keys are not interchangeable · use a key for the current access point",
    "settings.agentForm.testSuccess": "Connection successful",
    "settings.agentForm.testSuccessFetchedHarness": " · fetched {n} models",
    "settings.agentForm.testSuccessFetchedBorrow": " · fetched {n} models",
    "settings.agentForm.rawErrorToggle": "Show raw error",
    "settings.agentForm.moreOptions": "More options",
    "settings.agentForm.runModeLabel": "Runtime",
    "settings.agentForm.runModeHint":
      "Chosen automatically by the provider preset; no manual selection needed",
    "settings.agentForm.cancel": "Cancel",
    "settings.agentForm.save": "Save",
    "settings.agentForm.add": "Add",
    "settings.agentForm.error.nameRequired": "Name is required",
    "settings.agentForm.error.primaryModelRequired":
      "Primary model is required",
    "settings.agentForm.error.endpointRequired":
      "This agent requires an Endpoint",
    "settings.agentForm.error.saveFailed":
      "Save failed, please try again later",
    "settings.modelDropdown.custom": "Custom… (type a model name)",
    "settings.modelDropdown.placeholder": "Select a model",
    "settings.modelDropdown.live": "Live",
    "overview.title": "Overview",
    "overview.subtitle": "Cross-repo session fleet",
    "overview.sessionStats": "Session stats",
    "overview.empty":
      "No sessions yet. Pick a project from the bottom-left switcher to start.",
    "overview.needsAttention": "Needs Attention",
    "overview.running": "Running",
    "overview.idle": "Idle",
    "overview.repoCount": "repos",
    "overview.summary":
      "{attention} sessions need you · {running} running · across {repos} repos",
    "overview.summaryAttentionSuffix": "sessions need you",
    "overview.summaryRunningSuffix": "running",
    "overview.summaryRepos": "across {repos} repos",
    "overview.actionBand": "Needs your attention",
    "overview.recapBand": "Recent overview",
    "overview.allClear": "All clear. No sessions are waiting for you.",
    "overview.expandMore": "Show {n} more",
    "overview.collapse": "Collapse",
    "overview.localDefault": "Local Default",
    "overview.unknownRepo": "Unknown repo",
    "overview.team": "Agent Team",
    "overview.normal": "Normal",
    "overview.signal.pending": "Pending",
    "overview.signal.running": "Running",
    "overview.signal.recent": "Recent",
    "overview.folded": "Collapsed",
    "overview.activity": "Recent Activity",
    "overview.activityEmpty": "No changes recorded recently.",
    "overview.activityError": "Couldn't load recent activity. Try again later.",
    "overview.activityCommits": "{n} changes",
    "overview.activityFailed": "{n} failed",
    "overview.activityChartAria":
      "Daily recent activity bar chart. Bar height represents lines added and deleted.",
    "overview.activityTooltip":
      "{date}: {commits} commits / +{insertions} −{deletions} lines",
    "overview.activityTooltipFailed":
      "{date}: {commits} commits / +{insertions} −{deletions} lines / {failed} failed",
    "overview.activityFailureLegend":
      "A red dot marks a day with failed commits",
    "overview.usage": "Usage",
    "overview.usageHint":
      "Input + output total; cached input tokens are underreported.",
    "overview.usageTooltip": "{project}: {tokens} tokens, {percent}% of total",
    "overview.usageEmpty": "No usage data yet.",
    "overview.usageSessions": "{n} sessions",
    "overview.usageTopSessions": "Highest-usage sessions",
    "projectIntro.defaultSession": "Default session",
    "projectIntro.defaultPath":
      "No linked project · AgentLoom manages the working directory",
    "projectIntro.title": "Project overview",
    "projectIntro.defaultPlaceholder":
      "This default session has no project overview yet. AI will build a short summary here as you chat.",
    "projectIntro.tabsAria": "Project overview tabs",
    "projectIntro.tabIntro": "Overview",
    "projectIntro.tabDaily": "Daily",
    "projectIntro.rendered": "Rendered",
    "projectIntro.source": "Source",
    "projectIntro.aiAnalysis": "AI analysis",
    "projectIntro.repoPlaceholder":
      "No README.md yet. AI will build a project overview here as you chat.",
    "repoDoc.empty.title.intro": "This project has not been analyzed yet",
    "repoDoc.empty.desc.intro":
      "Let an agent read this repository and explain what it is, its tech stack, key directories, and recent work.",
    "repoDoc.empty.cta.intro": "Start AI analysis",
    "repoDoc.empty.title.daily": "There is no daily report for today",
    "repoDoc.empty.desc.daily":
      "Let an agent summarize recent commits, session output, and token usage into today's project report.",
    "repoDoc.empty.cta.daily": "Generate today's report",
    "repoDoc.readonly":
      "Read-only · the agent only reads and searches; it never modifies your files",
    "repoDoc.generating.title.intro": "Analyzing project",
    "repoDoc.generating.title.daily": "Generating today's report",
    "repoDoc.generating.lede":
      "You can leave this page; generation will continue in the background",
    "repoDoc.stale":
      "Generated from {sha} · the repository has newer commits · refresh available",
    "repoDoc.generatedAt": "Generated",
    "repoDoc.disclaimer": "AI-generated · may contain errors",
    "repoDoc.commit": "Based on commit {sha}",
    "repoDoc.error": "Generation failed",
    "repoDoc.retry": "Retry",
    "repoDoc.regenerate": "Regenerate",
    "repoDoc.loading": "Loading document…",
    "files.emptyTitle": "Open a session to browse project files",
    "files.emptyDesc":
      "Files shows the full project tree for the current session workspace.",
    "files.rendered": "Show rendered view",
    "files.source": "Show source",
    "files.find": "Find in file",
    "files.copyPath": "Copy path",
    "files.showTree": "Show file tree",
    "files.hideTree": "Hide file tree",
    "files.findPlaceholder": "Find in file…",
    "files.prevMatch": "Previous match",
    "files.nextMatch": "Next match",
    "files.loading": "Loading files…",
    "files.noPreview": "No previewable text file",
    "files.filterPlaceholder": "Filter files…",
    "files.directory": "Directory {path}",
    "files.open": "Open {path}",
    "files.expandDirectory": "Expand directory {path}",
    "files.collapseDirectory": "Collapse directory {path}",
    "files.openFile": "Open file {path}",
    "files.truncated": "Large project — showing the first {max} entries only",
    "preview.empty": "Select a file to preview",
    "preview.loading": "Loading…",
    "preview.error": "Can't open",
    "preview.truncated": "truncated to 256 KB",
    "preview.imageUnavailable": "Image preview unavailable",
    "preview.binary": "Binary file — can't preview",
    "rightPanel.soon.side": "Side chat is coming soon.",
    "rightPanel.soon.terminal": "Terminal is coming soon.",
    "rightPanel.soon.browser": "Browser is coming soon.",
    "rightPanel.picker.hint": "Open a tool in a tab",
    "rightPanel.picker.open": "Open {name}",
    "rightPanel.picker.unavailable": "{name} coming soon",
    "rightPanel.picker.soon": "Soon",
    "rightPanel.picker.previewLabel": "Preview",
    "rightPanel.picker.filesDescription":
      "Browse project files · select to preview",
    "rightPanel.picker.reviewDescription": "View the current diff",
    "rightPanel.picker.sideDescription": "Start a side conversation",
    "rightPanel.picker.terminalDescription": "Open an interactive shell",
    "rightPanel.picker.browserDescription": "Open a web page",
    "rightPanel.preview.close": "Close preview",
    "rightPanel.empty.noSessionTitle": "Open a session to review",
    "rightPanel.empty.noChangesTitle": "No changes",
    "rightPanel.empty.noSessionDescription":
      "Open or create a session to review the agent's changes here.",
    "rightPanel.empty.noChangesDescription":
      "This session has no agent changes to review yet.",
    "rightPanelTabs.expand": "Expand right panel",
    "rightPanelTabs.expandTitle": "Expand right panel ⌘J",
    "rightPanelTabs.newTab": "New tab / tool picker",
    "rightPanelTabs.restore": "Restore split view",
    "rightPanelTabs.restoreTitle": "Restore right panel to sidebar",
    "rightPanelTabs.maximize": "Maximize panel",
    "rightPanelTabs.maximizeTitle":
      "Maximize right panel in main area; keep sidebar visible",
    "rightPanelTabs.collapse": "Collapse right panel",
    "rightPanelTabs.collapseTitle": "Collapse right panel ⌘J",
    "reviewPanel.title": "Changes · {count} files",
    "reviewPanel.close": "Close",
    "reviewPanel.noUndoRecord": "No undo record · cannot undo",
    "reviewPanel.dataFileNotShown": "Data file · not shown",
    "reviewPanel.showMore": "Show more",
    "reviewPanel.statusCommittedAndUncommitted":
      "{committed} file(s) committed · {uncommitted} uncommitted",
    "reviewPanel.statusCommittedOnly": "{committed} file(s) committed",
    "reviewPanel.statusUncommittedOnly": "{uncommitted} file(s) uncommitted",
    "reviewPanel.unavailableTitle": "Unable to generate a diff",
    "reviewPanel.unavailableDescription":
      "This project is not a Git working tree with a HEAD commit yet. Make one commit and changes will show up here.",
    "reviewPanel.otherDirty":
      "{count} other change(s) in the working directory are not part of this review",
    "undoPanel.checklist.aria": "Undo checklist for this turn",
    "undoPanel.checklist.title": "Changes this turn · {count} files",
    "undoPanel.checklist.mode": "This turn only",
    "undoPanel.result.aria": "Undo result for this turn",
    "undoPanel.result.title": "Undo result · {count} files",
    "undoPanel.result.mode": "After undo",
    "undoPanel.result.subtitle": "Per-file undo result for this turn",
    "undoPanel.back": "Exit this-turn view",
    "undoPanel.loading": "Loading this turn's changes…",
    "undoPanel.loadFailed": "Could not load the undo checklist: {reason}",
    "undoPanel.empty": "This turn has no edit-tool changes to undo.",
    "undoPanel.allStale":
      "Every record in this turn is outdated: the files were committed again later, and undoing would overwrite those commits, so none of them can be selected.",
    "undoPanel.selectFile": "Select {path}",
    "undoPanel.kind.created": "Created",
    "undoPanel.kind.modified": "Modified",
    "undoPanel.kind.deleted": "Deleted",
    "undoPanel.file.modified":
      "Modified · undo restores the content from before this turn",
    "undoPanel.file.created": "Created · undo will delete this file",
    "undoPanel.file.deleted": "Deleted · undo will restore this file",
    "undoPanel.file.binary":
      "Binary file, can't preview · still selectable for undo",
    "undoPanel.file.tooLarge":
      "File is too large ({size}), can't preview · still selectable for undo",
    "undoPanel.file.unsupported": "Can't preview · still selectable for undo",
    "undoPanel.file.alreadyUndone": "Already undone · can't select again",
    "undoPanel.file.stale":
      "This record is outdated: the file was committed again after this — undoing would overwrite that commit · can't select",
    "undoPanel.badge.binary": "Binary",
    "undoPanel.diff.modified": "Before changes → now",
    "undoPanel.diff.created": "Missing before changes → now",
    "undoPanel.diff.deleted": "Before changes → missing now",
    "undoPanel.boundary.title":
      "Undo only covers files the agent changed with editing tools.",
    "undoPanel.boundary.terminalPrefix": "Terminal actions (",
    "undoPanel.boundary.rm": "rm",
    "undoPanel.boundary.separator": " / ",
    "undoPanel.boundary.sed": "sed -i",
    "undoPanel.boundary.terminalSuffix":
      " / scripts / redirection) are not covered and cannot be rolled back.",
    "undoPanel.undoSelected": "Undo {count} selected files",
    "undoPanel.undoing": "Undoing…",
    "undoPanel.undoFailed": "Undo failed: {reason}",
    "undoPanel.result.restored": "Restored {count} files",
    "undoPanel.result.skipped": "Not restored: {count} files",
    "undoPanel.result.skippedDetail":
      "See each file for the exact reason; its current contents were not overwritten.",
    "undoPanel.result.failed": "Failed: {count}",
    "undoPanel.result.file.restored":
      "Undone · restored to the content from before this turn",
    "undoPanel.result.file.createdRestored":
      "Undone · newly created file deleted",
    "undoPanel.result.file.deletedRestored": "Undone · deleted file restored",
    "undoPanel.result.file.skippedChanged":
      "Not restored · it changed after you reviewed it",
    "undoPanel.result.file.skippedUnsafe":
      "Not restored · this path cannot be accessed safely now",
    "undoPanel.result.file.skippedAlreadyUndone": "Already undone earlier",
    "undoPanel.result.file.skippedStale":
      "Not restored · this record is outdated; undoing would overwrite a later commit",
    "undoPanel.result.file.skippedUnknown":
      "Not restored · backend reason: {reason}",
    "undoPanel.result.file.failed": "Not restored · undo failed: {reason}",
    "undoPanel.result.badge.restored": "Restored",
    "undoPanel.result.badge.deleted": "Deleted",
    "undoPanel.result.badge.skipped": "Not restored",
    "undoPanel.result.badge.failed": "Failed",
    "undoPanel.result.changedDiff":
      "Content changed during the pre-undo safety check",
    "inlineDiffCard.openInReview": "Open in Review",
    "stream.role.lead": "Lead",
    "stream.role.user": "You",
    "quote.role.assistant": "Assistant",
    "stream.status.workingAria": "{name} is working",
    "stream.status.working": "Working",
    "stream.status.lastStep": "Last step: {summary}",
    "stream.status.thinking": "Thinking",
    "stream.status.silent": "Silent for {seconds}s",
    "stream.status.longTask": "Long-running engine task in progress",
    "stream.status.waitingOnWorker": "Waiting on worker: {name}",
    "stream.status.waitingOnWorkers":
      "Waiting on {count} workers: {name} and others",
    "stream.task.view": "View",
    "stream.worker.badge.stopped": "STOPPED",
    "runCard.state.undone": "Turn undone",
    "runCard.state.partial": "Undone {undone} / {total}",
    "runCard.state.completed": "Completed",
    "runCard.changesAria": "Turn changes",
    "runCard.summary": "Files changed this turn: {files}",
    "runCard.interrupted": " · Interrupted",
    "runCard.view": "View",
    "runCard.undo": "Undo…",
    "runCard.continueUndo": "Continue undo…",
    "runCard.viewResult": "View result",
    "runCard.result.restored": "Restored this time: {count}",
    "runCard.result.skipped": "Not restored this time: {count}",
    "runCard.result.failed": "Failed this time: {count}",
    "runCard.result.unselected": "Not selected this time: {count}",
    "runCard.partialNote.both":
      "This time, {skipped} were not restored and {failed} failed; view each file for the exact reason",
    "runCard.partialNote.skipped":
      "This time, {count} files were not restored; view the result for the exact reasons",
    "runCard.partialNote.failed":
      "Undo failed for {count} files this time; view the result",
    "runLeadTurn.fallbackLeadName": "Lead",
    "runLeadTurn.captain": "· Lead",
    "runLeadTurn.processSummary": "Process · Tasks: {count}",
    "runLeadTurn.viewProcess": "View process",
    "taskStack.undoRun": "Undo this run",
    "liveStreamCard.running": "Running",
    "liveStreamCard.preparing": "Preparing…",
    "liveStreamCard.isolated": "Isolated",
    "memberDrillIn.noTokens": "No tokens",
    "memberDrillIn.status.running": "Running",
    "memberDrillIn.status.needsInput": "Awaiting input",
    "memberDrillIn.status.done": "Done",
    "memberDrillIn.status.failed": "Failed",
    "memberDrillIn.status.stopped": "Stopped",
    "memberDrillIn.criterion.pending": "Pending",
    "memberDrillIn.criterion.passed": "Passed",
    "memberDrillIn.criterion.failed": "Failed",
    "memberDrillIn.criterion.waived": "Waived",
    "memberDrillIn.criterion.uncertain": "Needs review",
    "memberDrillIn.backToLead": "Back to Lead",
    "memberDrillIn.steps": "Step {done}/{total} · {tokens}",
    "memberDrillIn.stopAria": "Stop {name}",
    "memberDrillIn.stop": "⏹ Stop",
    "memberDrillIn.failureReason": "Failure reason",
    "memberDrillIn.overview": "Overview",
    "memberDrillIn.taskDetails": "Task details",
    "memberDrillIn.goal": "Goal",
    "memberDrillIn.acceptance": "Acceptance",
    "memberDrillIn.changedFiles": "Changed files",
    "memberDrillIn.changedFilesCaveat":
      "The changed-files list comes from the checkpoint ledger; direct terminal writes (shell redirection, sed, etc.) may not be recorded.",
    "memberDrillIn.verification": "Verification",
    "memberDrillIn.exitCode": "Exit code {code}",
    "memberDrillIn.viewAssignment": "View assignment",
    "memberDrillIn.rawTrace": "Raw trace",
    "messageContent.image": "[Image]",
    "messageContent.imageLoading": "Loading image…",
    "messageContent.imageLoadFailed": "[Image failed to load]",
    "messageContent.imageArtifact.preview": "Preview image {name}",
    "messageContent.imageMenu.label": "Image actions",
    "messageContent.imageMenu.copyImage": "Copy image",
    "messageContent.imageMenu.copyPath": "Copy full path",
    "messageContent.imageMenu.imageUnavailable":
      "Copying images is unavailable in this environment",
    "messageContent.imageMenu.imageCopied": "Image copied",
    "messageContent.imageMenu.pathCopied": "Path copied",
    "messageContent.imageMenu.copyFailed": "Copy failed",
    "messageContent.html.openExternal": "Open {name} in browser",
    "lightbox.label": "Enlarged image preview",
    "lightbox.close": "Close image preview",
    "lightbox.imageAlt": "Enlarged image",
    "lightbox.loading": "Loading full-size image…",
    "lightbox.loadFailed": "Image failed to load",
    "messageContent.gate.proposing": "Lead is drafting a plan…",
    "messageActions.copied": "Copied",
    "messageActions.copy": "Copy",
    "messageActions.exportMarkdown": "Export markdown",
    "messageActions.quote": "Quote",
    "messageMarkdown.toolStatusWithExit": "[{status} exit {exitCode}]",
    "messageMarkdown.toolStatus": "[{status}]",
    "messageMarkdown.image": "![image](attachment:{attachmentId})",
    "messageMarkdown.teamRun": "[Agent Team · {n} subtasks ({names})]",
    "messageMarkdown.runCard":
      "[Turn changes: {n} files (+{insertions} −{deletions})]",
    "messageMarkdown.approval": "[Approval {status}: {tool} · {command}]",
    "messageMarkdown.scopeChange": "[Agent proposed a scope change]",
    "messageMarkdown.leadSummary": "[Lead summary · {source}]",
    "messageMarkdown.codingTask": "[Coding task · {phase}]",
    "messageMarkdown.gateCard": "[Plan draft]",
    "messageMarkdown.draftFailed": "[Draft failed]",
    "messageMarkdown.dispatchCard": "\n[Task: {name} · {sub}]\n",
    "messageMarkdown.decisionCard": "[Decision card]",
    "messageMarkdown.runTerminalWithMessage": "[{status} · {message}]",
    "messageMarkdown.runTerminal": "[{status}]",
    "thinking.collapse": "Collapse",
    "thinking.expand": "Expand",
    "codeBlock.openInBrowser": "Open in browser",
    "codeBlock.openTemporaryHtml": "Open temporary HTML",
    "codeBlock.copied": "Copied",
    "codeBlock.copy": "Copy",
    "codeBlock.collapse": "Collapse",
    "codeBlock.expandLines": "Expand +{n} lines",
    "toolCard.status.running": "Running",
    "toolCard.status.done": "Done",
    "toolCard.status.failed": "Failed",
    "toolCard.status.interrupted": "Interrupted",
    "toolCard.hiddenLinesAbove": "+ {n} lines above",
    "toolCard.name.bash": "Run command",
    "toolCard.name.read": "Read file",
    "toolCard.name.write": "Write file",
    "toolCard.name.edit": "Edit file",
    "toolCard.name.glob": "Find files",
    "toolCard.name.grep": "Search",
    "toolCard.name.task": "Subtask",
    "toolCard.name.todoWrite": "Update todos",
    "toolCard.name.webFetch": "Fetch page",
    "toolCard.name.webSearch": "Search web",
    "toolCard.name.notebookEdit": "Edit notebook",
    "toolCard.name.bashOutput": "View command output",
    "toolCard.name.killShell": "Stop command",
    "toolCard.name.ls": "List directory",
    "toolCard.name.memory": "Save note",
    "toolCard.name.imageGen": "Generate image",
    "toolCard.name.commit": "Commit code",
    "toolCard.name.push": "Push code",
    "toolCard.name.createPr": "Create PR",
    "toolCard.name.publish": "Publish",
    "toolCard.name.verifier": "Verify",
    "inspector.status": "Status",
    "inspector.owner": "Owner",
    "inspector.artifacts": "Artifacts",
    "inspector.failureReason": "Failure reason",
    "inspector.stderrTail": "stderr output (tail)",
    "inspector.toolTrace": "Activity",
    "inspector.noOutput": "No output yet",
    "inspector.close": "Close",
    "inspector.statusLabel.running": "Running",
    "inspector.statusLabel.needs_input": "Awaiting input",
    "inspector.statusLabel.done": "Done",
    "inspector.statusLabel.failed": "Failed",
    "inspector.statusLabel.stopped": "Stopped",
    "inspector.filesUnit": "{n} files",
    "stream.toolFold.steps": "Ran {n} steps",
    "runTerminal.completed": "Completed",
    "runTerminal.error": "Error",
    "runTerminal.interrupted": "Interrupted",
    "runTerminal.blocked": "Blocked",
    "runTerminal.needsDecision": "Awaiting decision",
    "runTerminal.fallback": "Wrap-up incomplete · state recovered via fallback",
    "stopReason.blockedQuestions": "The lead is waiting on an open question",
    "stopReason.noProgress":
      "Stopped automatically after several turns with no real progress",
    "stopReason.stuckRepeating":
      "Stopped by the safety net after repeating the same action",
    "stopReason.budgetExhaustedStillProgressing":
      "This round's turn budget ran out (still making progress) — send a message to continue",
    "stopReason.contextBudgetExhausted":
      "Context window is full, so it stopped — send a message to continue",
    "stopReason.approvalUnavailable": "Approval channel unavailable — stopped",
    "stopReason.rejectedRepeatedly":
      "Repeated submissions failed acceptance — stopped",
    "app.run.stoppedPendingQuestion":
      "\nThere's still a question waiting for your answer — click an option above to continue.",
    "time.justNow": "just now",
    "time.minAgo": "{n} min ago",
    "time.hourAgo": "{n} hr ago",
    "time.dayAgo": "{n} d ago",
    "codingTask.phase.finalizing": "Finalizing changes",
    "codingTask.phase.askVerify": "Confirm verification command",
    "codingTask.phase.verifying": "Verifying",
    "codingTask.phase.verifyFailed": "Verification failed",
    "codingTask.phase.askApply": "Legacy apply confirmation",
    "codingTask.phase.merging": "Merging into staging",
    "codingTask.phase.applying": "Awaiting your decision",
    "codingTask.phase.applied": "Applied",
    "codingTask.phase.landingBlocked": "Blocked",
    "codingTask.phase.shelved": "Shelved",
    "codingTask.phase.error": "Error",
    "taskStatus.chip.steps": "{done}/{total}",
    "taskStatus.chip.files": "{n} files",
    "taskStatus.chip.verify": "{n} checks",
    "taskStatus.phase.askApplyProgress":
      "Legacy apply confirmation (rerun or leave for now)",
    "taskStatus.phase.applyingProgress": "Changes are in isolation",
    "taskStatus.phase.landingBlockedProgress": "Pre-apply checks failed",
    "codingTask.why": "Why",
    "codingTask.details": "Details",
    "codingAsk.verifyFailed": "Verification failed",
    "codingAsk.command": "Command",
    "codingAsk.retryWithCommand": "Change command and retry",
    "codingAsk.viewChanges": "View changes",
    "codingAsk.shelve": "Leave for now",
    "codingAsk.verifyPrompt":
      "Use the following command to verify these changes. You can edit it:",
    "codingAsk.startVerify": "Start verification",
    "scopeChange.kind.scope": "Scope",
    "scopeChange.kind.objective": "Objective",
    "scopeChange.kind.constraint": "Constraint",
    "scopeChange.continueDraft":
      "Continue from the previous turn and adopt these scope changes:\n{changes}",
    "scopeChange.collapsedTitle": "The agent proposed a scope change",
    "scopeChange.collapsedStatus": "Collapsed",
    "scopeChange.expand": "Show proposal",
    "scopeChange.title.multi":
      "Agent proposed {count} changes to the task boundaries",
    "scopeChange.title.single": "Agent proposed a scope change",
    "scopeChange.pending": "Awaiting your decision",
    "scopeChange.description.multi":
      "The agent proposed {count} boundary changes for your approval.",
    "scopeChange.description.single":
      "The agent paused mid-task to propose a scope change. Accept the proposal to continue, or collapse it and give different instructions.",
    "scopeChange.finalizeNote":
      "This turn has ended. Some files may have changed and were saved as usual; the scope change has not been applied.",
    "scopeChange.continueHint":
      "Accept and continue starts the next turn with the agent's proposed boundaries.",
    "scopeChange.collapse": "Collapse",
    "scopeChange.acceptAndContinue": "Accept and continue",
    "composer.permission.label": "Permission: Auto · Current version Auto-only",
    "composer.permission.trustBase":
      "Trust-based · Review/revoke post-execution",
    "composer.permission.autoOnly": "Current version Auto-only",
    "composer.permission.shortLabel": "Permission",
    "composer.attachment.label": "Attach file",
    "composer.attachment.remove": "Remove attachment",
    "composer.attachment.imageAlt": "attached image",
    "composer.attachment.comingSoon": "Still in planning",
    "composer.voice.label": "Voice",
    "composer.voice.comingSoon": "Still in planning",
    "composer.usage.total": "total",
    "composer.readonly.continued":
      "Session handed off to a new session · read-only · continue in the new session",
    "composer.quote.clear": "Clear quote",
    "composer.pendingDecision.label": "Something needs your confirmation",
    "composer.input.placeholder": "Type a message…",
    "composer.stop": "Stop",
    "composer.send": "Send",
    "composer.status.membersWorking": "Team members working…",
    "composer.memberActiveHint":
      "A member task is still running. Wait for it to finish or stop it from its card before sending.",
    "composer.memberRecheckFailedHint":
      "Could not verify the member task status. Please try again.",
    "composer.hint.send": "Enter to send · Shift+Enter for a new line",
    "composer.agentSelector.loadingSuffix": ", loading",
    "composer.agentSelector.trigger.team":
      "Select agent: Lead {name}, team members: {count}{loading}",
    "composer.agentSelector.trigger.solo": "Select agent: {name}{loading}",
    "composer.agentSelector.description.canLead":
      "{provider} · can lead + delegate",
    "composer.agentSelector.description.unavailable":
      "{provider} · member only",
    "composer.agentSelector.role.lead": "Lead",
    "composer.agentSelector.members.count": "Members {count}",
    "composer.agentSelector.title.team": "Who should run this session",
    "composer.agentSelector.title.solo": "Select agent",
    "composer.agentSelector.auto.teamUnavailableTitle":
      "Auto is for Solo selection only · unavailable while a Lead is set",
    "composer.agentSelector.auto.teamUnavailable":
      "Unavailable in Team mode (remove the Lead to restore)",
    "composer.agentSelector.empty":
      "No agents available · configure one in Settings",
    "composer.agentSelector.action.cancelLead": "Remove Lead {name}",
    "composer.agentSelector.action.setLead": "Set {name} as Lead",
    "composer.agentSelector.memberAria": "Member {name}",
    "composer.agentSelector.foot":
      "Crown = set the Lead (single choice); member toggles choose workers. Remove the Lead to return to Solo.",
    "composer.agentSelector.manage": "Manage agents →",
    "teamBar.roleLead": "Lead",
    "teamBar.barMembers": "{n} members",
    "teamBar.barRunning": "{running} members running · {total} total",
    "teamBar.expand": "Expand",
    "teamBar.collapse": "Collapse",
    "teamBar.panelTitle":
      "Team setup · Session-level (changes persist for this session)",
    "teamBar.leadHead": "Lead (team lead · primary conversation partner)",
    "teamBar.leadCantBe": "Can't be Lead yet · available as a member",
    "teamBar.rosterHead":
      "Members (available participants · Lead selects as needed · not everyone runs)",
    "teamBar.memberAria": "Member {name}",
    "teamBar.capHint.claude": "Leading / general",
    "teamBar.capHint.codex": "Implementation / testing",
    "teamBar.capHint.gemini": "Multimodal / writing",
    "teamBar.capHint.kimi": "Long-form summaries",
    "teamBar.capHint.deepseek": "Quick search / low cost",
    "gateCard.readonlyAssignments": "Assignments · read-only",
    "gateCard.autoDispatch": "Auto-assigned",
    "gateCard.manualDispatch": "Manually assigned",
    "gateCard.unassigned": "Unassigned",
    "gateCard.manualIntro": "Add a goal and acceptance criteria, then start.",
    "gateCard.autoIntro":
      "I broke this down into a goal, {count} acceptance criteria, and assignments. Review it, make any changes, then start.",
    "gateCard.draft": "Draft",
    "gateCard.headerTitle": "Goal · approving dispatches the team",
    "gateCard.tierNote": "Review it · make any changes · then start.",
    "gateCard.goalLabel": "Goal (confirm the Lead understood you)",
    "gateCard.editGoalAria": "Edit goal",
    "gateCard.emptyGoal": "(Not set)",
    "gateCard.edit": "Edit",
    "gateCard.acceptanceTitle": "Acceptance criteria",
    "gateCard.acceptanceHint": "· Focus here: you decide what done means",
    "gateCard.criterionAria": "Acceptance criterion {index}",
    "gateCard.criterionPlaceholder": "Add an acceptance criterion…",
    "gateCard.deleteCriterionAria": "Delete this acceptance criterion",
    "gateCard.showRemaining": "Show {count} more",
    "gateCard.addCriterion": "+ Add acceptance criterion",
    "gateCard.assignments": "Assignments",
    "gateCard.assignmentHint":
      " · Lead matched work to strengths (open to adjust)",
    "gateCard.freezing": "Starting…",
    "gateCard.confirmAndStart": "Confirm and start",
    "gateCard.start": "Start",
    "gateCard.redraft": "Ask Lead to redraft",
    "gateCard.readonlyCannotStart": "Cannot start in read-only mode",
    "gateCard.freezeHint": "Starts the plan and dispatches the team",
    "approvalCard.approvedCriterion": "Accepted",
    "approvalCard.approvedCommand": "Allowed",
    "approvalCard.approvedCriterionNote":
      "You accepted this criterion proposal",
    "approvalCard.approvedCommandNote": "You allowed this command · running",
    "approvalCard.rejectedCriterion": "Declined",
    "approvalCard.rejectedCommand": "Denied",
    "approvalCard.rejectedCriterionNote":
      "You declined this criterion proposal · agent notified",
    "approvalCard.rejectedCommandNote":
      "You denied this command · tool failure sent to the agent",
    "approvalCard.cancelled": "Cancelled",
    "approvalCard.cancelledNote": "Session ended · approval cancelled",
    "approvalCard.pendingCriterionTitle": "Proposed acceptance criterion",
    "approvalCard.pendingCommandTitle": "Approval required",
    "approvalCard.pendingLabel": "Awaiting decision",
    "approvalCard.criterionProposal": "Criterion proposal",
    "approvalCard.criterionLabel": "Criterion",
    "approvalCard.commandLabel": "Command",
    "approvalCard.directory": "Directory",
    "approvalCard.criterionHint":
      "Accept to add this criterion to the current goal. Decline to discard it and let the agent continue.",
    "approvalCard.commandHint":
      "Allow to run this command in the workspace. Deny to fail the tool call and let the agent continue.",
    "approvalCard.denyCriterion": "Decline",
    "approvalCard.denyCommand": "Deny",
    "approvalCard.allowCriterion": "Accept",
    "approvalCard.allowCommand": "Allow",
    "assignmentEditor.title":
      "Assignments · Lead chose a starting point; you can adjust it",
    "assignmentEditor.autoDispatch": "Auto-assign",
    "assignmentEditor.reassignAria": "Reassign / change model",
    "assignmentEditor.unassigned": "Unassigned",
    "assignmentEditor.reassignTo": "Reassign to",
    "assignmentEditor.availabilityNote":
      "Only enabled, currently available agents are shown · assignments to disabled or missing agents are blocked · finer repo / namespace constraints are coming later",
    "assignmentEditor.removeMember": "Remove this member",
    "assignmentEditor.addTask": "+ Add a task",
    "assignmentEditor.leadValidationNote":
      "Local validation / review is handled by Lead (Claude)",
    "agentDropdown.selectAria": "Select agent",
    "agentDropdown.title": "Select agent (single choice)",
    "agentDropdown.empty": "No agents available · configure one in Settings",
    "agentDropdown.manage": "Manage agents →",
    "modeDropdown.select": "Select mode: {label}",
    "modeDropdown.current": "Current",
    "modeDropdown.normal.label": "Normal · single agent",
    "modeDropdown.normal.description":
      "You and the right partner, focused on the task at hand",
    "modeDropdown.collaboration": "Multi-agent collaboration",
    "modeDropdown.team.description":
      "Lead with members · delegates automatically",
    "modeDropdown.round.description": "Facilitated peer brainstorm",
    "modeDropdown.soonTitle": "{label} (coming soon)",
    "modeDropdown.soon": "Coming soon",
    "leadSummary.status.failed": "Not completed · {succeeded}/{total}",
    "leadSummary.status.partial": "Partially completed · {succeeded}/{total}",
    "leadSummary.advice.rateLimit":
      "Try dispatching again with a model that has available quota, or retry later.",
    "leadSummary.advice.default":
      "Try dispatching again with an available worker. To continue yourself, switch back to Normal.",
    "leadSummary.failure.withReason": "Worker failed: {reason}",
    "leadSummary.failure.stalled": "Worker stalled: {reason}",
    "leadSummary.failure.budgetExhausted": "Worker ran out of budget: {reason}",
    "leadSummary.failure.contextExhausted":
      "Worker ran out of context: {reason}",
    "leadSummary.failure.noResult": "Worker failed: No usable result returned",
    "memberFailure.reason.quota": "API quota/rate limit",
    "memberFailure.reason.localCodexMcpAuth":
      "Local Codex/MCP authentication failed",
    "memberFailure.reason.auth": "API authentication failed",
    "memberFailure.reason.overload": "API service busy/overloaded",
    "memberFailure.reason.stalled":
      "Worker stalled: waiting on an answer or blocked, not an environment failure",
    "memberFailure.reason.budgetExhausted":
      "Worker ran out of its turn budget: it was still making normal progress, not stuck",
    "memberFailure.reason.contextExhausted":
      "Worker's context window couldn't fit the conversation: single-turn token budget exhausted, not stuck",
    "memberFailure.reason.env": "Worker process/environment failure",
    "memberFailure.reason.spawn": "Worker invocation failed",
    "memberFailure.reason.noFinalText": "Worker returned no result",
    "memberFailure.code.blockedQuestions":
      "Stopped waiting on an open question",
    "leadSummary.workerFailure.trace": "Worker failed: {reason} (see trace)",
    "leadSummary.workerFailure.stalledTrace":
      "Worker stalled: {reason} (see trace)",
    "leadSummary.workerFailure.budgetExhaustedTrace":
      "Worker ran out of budget: {reason} (see trace)",
    "leadSummary.workerFailure.contextExhaustedTrace":
      "Worker ran out of context: {reason} (see trace)",
    "leadSummary.workerFailure.noResultTrace":
      "Worker returned no result (see trace)",
    "leadSummary.workerFailure.emptyPassthroughTrace":
      "(Worker produced no text; see trace)",
    "leadSummary.workerFailure.emptyFallbackTrace": "(No text; see trace)",
    "leadSummary.section.changes": "Changes",
    "leadSummary.section.verify": "Verification",
    "leadSummary.section.risk": "Risks",
    "leadSummary.section.fallback":
      "{name} (synthesis failed; original output)",
    "leadSummary.section.changes.table":
      "| File | What changed | Diff |\n| --- | --- | --- |\n{rows}",
    "leadSummary.section.verify.command": "- `{cmd}` (exit code {code})",
    "leadSummary.coding.applied":
      "Changes have been applied to the current branch.",
    "leadSummary.coding.landingBlocked":
      "Changes were not applied automatically because reliable verification is missing or a safety gate did not pass. Review the changes first, then continue when ready.",
    "leadSummary.coding.shelved": "Changes were shelved (not applied).",
    "leadSummary.coding.verify.verdict": "- `{cmd}` ({verdict})",
    "leadSummary.coding.verify.executed": "- `{cmd}` (executed)",
    "leadSummary.finding.failure": "{name}: {reason}",
    "leadSummary.trust.insufficientEvidence":
      "Needs verification · insufficient evidence",
    "leadSummary.trust.commandTrace": "Command trace",
    "leadSummary.trust.workerReport": "Worker self-report",
    "leadSummary.trust.waived": "Skipped",
    "leadSummary.trust.unverified": "Needs verification",
    "leadSummary.pending": "Lead is synthesizing the team’s output…",
    "leadSummary.stopped.status": "Stopped",
    "leadSummary.stopped.message":
      "This worker has been stopped. Tell me what to change next.",
    "leadSummary.findings.done": "Completed",
    "leadSummary.findings.miss": "Not completed",
    "leadAsk.rationale": "Why: {rationale}",
    "decisionCard.recommended": "Recommended",
    "decisionCard.hint": "Pick an option to reply",
    "decisionCard.questionExpand": "Show question ▾",
    "decisionCard.questionCollapse": "Collapse ▴",
    "decisionCard.chosen": "Chose: {option}",
    "decisionCard.rationaleToggle": "Why ask first {indicator}",
    "decisionCard.retry": "Retry",
    "draftFailed.parseExhausted":
      "Lead tried {attempts} times but could not produce a usable structured plan ({lastError}).",
    "draftFailed.invokeFailed":
      "Lead failed while drafting the plan: {reason}.",
    "draftFailed.title": "Lead draft failed",
    "draftFailed.retry": "Retry draft",
    "draftFailed.manual": "Fill gate manually",
    "draftFailed.backToNormal": "Back to Normal",
    "sidebar.newSessionDisabledTitle":
      "Add a repo first · sessions require a repo in this namespace",
    "sidebar.newSessionTitle": "New session",
    "sidebar.collapse": "Collapse sidebar",
    "sidebar.back": "Back",
    "sidebar.forward": "Forward",
    "sidebar.overview": "Overview",
    "sidebar.overviewTitle": "Overview home",
    "sidebar.search": "Search",
    "sidebar.searchTitle": "Search / Command ⌘K (coming soon)",
    "sidebar.projectIntro": "Project overview",
    "sidebar.sessions": "Sessions",
    "sidebar.newSession": "＋ New session",
    "sidebar.groupNamePlaceholder": "Group name…",
    "sidebar.newGroup": "＋ New group",
    "sidebar.archived": "Archived ({n})",
    "sidebar.resize": "Drag to resize sidebar",
    "sessionRow.pinned": "Pinned",
    "sessionRow.saveShort": "save",
    "sessionRow.cancelShort": "cancel",
    "sessionRow.unread": "Unread",
    "sessionRow.rename": "Rename",
    "sessionRow.more": "More",
    "sessionGroup.rename": "Rename",
    "sessionGroup.delete": "Delete group",
    "sessionMenu.back": "‹ Back",
    "sessionMenu.ungrouped": "Ungrouped",
    "sessionMenu.newGroup": "＋ New group…",
    "sessionMenu.groupNamePlaceholder": "Group name…",
    "sessionMenu.unpin": "Unpin",
    "sessionMenu.pin": "Pin",
    "sessionMenu.markRead": "Mark as read",
    "sessionMenu.markUnread": "Mark as unread",
    "sessionMenu.rename": "Rename",
    "sessionMenu.moveContinuationGroup": "Move continuation group ▸",
    "sessionMenu.moveToGroup": "Move to group ▸",
    "sessionMenu.restoreContinuationGroup": "Restore continuation group",
    "sessionMenu.restore": "Restore",
    "sessionMenu.archiveContinuationGroup": "Archive continuation group",
    "sessionMenu.archive": "Archive",
    "sessionMenu.stopBeforeDelete": "Stop the session before deleting",
    "sessionMenu.delete": "Delete",
    "continuation.panel.label": "Handoff draft",
    "continuation.panel.headerTitle": "Handoff draft",
    "continuation.panel.generated": "Auto-generated",
    "continuation.panel.editable": "Editable",
    "continuation.panel.parent": "Parent session",
    "continuation.panel.turns": "{n} turns",
    "continuation.panel.cancel": "Cancel",
    "continuation.panel.start": "Start child session",
    "continuation.panel.starting": "Starting…",
    "continuation.panel.retry": "Re-summarize",
    "continuation.panel.editToggle": "Edit",
    "continuation.panel.doneEditing": "Done editing",
    "continuation.panel.startDisabledHint":
      "Fill in Goal and Next step to start",
    "continuation.panel.v3.loading": "Generating handoff document…",
    "continuation.panel.v3.suggestedTitleLabel": "Suggested session name",
    "continuation.panel.v3.editToggle": "Edit",
    "continuation.panel.v3.doneEditing": "Done editing",
    "continuation.panel.v3.warningsLabel": "Notices",
    "continuation.panel.v3.errorBackend": "Backend error: ",
    "continuation.panel.v3.errorKey": "Key error: ",
    "continuation.panel.v3.errorParser": "Parse error: ",
    "continuation.panel.v3.errorBusy":
      "Previous generation still in progress, please wait and retry",
    "continuation.panel.v3.loadingSub":
      "Reading session history and summarizing, may take tens of seconds",
    "continuation.panel.v3.retry": "Retry",
    "continuation.panel.v3.startDisabledHint":
      "Handoff document cannot be empty",
    "continuation.panel.v3.readOnly": "Read-only",
    "continuation.menu.handover": "Hand off to new session",
    "continuation.menu.disabled.archived":
      "Archived sessions cannot be handed off",
    "continuation.menu.disabled.running": "Stop the run before handing off",
    "continuation.menu.disabled.continued": "Session already handed off",
    "continuation.menu.disabled.assembling": "Generating handoff document",
    "continuation.notice.ready": "Handoff draft is ready: {title}",
    "continuation.lineage.parentBadge": "Handed off to →",
    "continuation.lineage.childBadge": "↳ Continued from {title}",
    "continuation.lineage.childTooltip": "Continued from {title}",
    "continuation.lineage.fallbackParent": "parent session",
    "topbar.tasks.view": "View background tasks",
    "topbar.tasks.count": "{n} background tasks",
    "surfaceHeader.expandSidebar": "Expand session sidebar",
    "surfaceHeader.back": "Back",
    "surfaceHeader.forward": "Forward",
    "surfaceHeader.overview": "Overview",
    "surfaceHeader.overviewTitle": "Overview home",
    "goalBar.done": "Done",
    "goalBar.label": "Goal",
    "goalBar.criteriaCount": "Criteria · {total}",
    "goalBar.pendingReview": "Run complete · Needs review: {count}",
    "goalBar.viewCriteria": "View criteria",
    "goalCriteriaPanel.goal": "Goal",
    "goalCriteriaPanel.criteria": "Acceptance criteria",
    "goalCriteriaPanel.empty": "No acceptance criteria for this turn",
    "sessionMain.finished": "All done ✓",
    "sessionContextBar.menu": "Session menu",
    "tasklist.stop": "Stop · coming in block 2",
    "inspector.backToList": "Back to tasks",
    "lead.crown.disabledTip":
      "This engine can't be a lead yet (codex support coming soon)",
    "lead.error.claudeOnly":
      "Only Claude can be Lead. Please switch the Lead back to Claude.",
    "app.dispatch.confirm": "Confirm dispatch",
    "app.dispatch.cancel": "Cancel",
    "app.interrupt.label": "Previous run interrupted (after restart)",
    "app.interrupt.redispatch": "Redispatch from scratch ›",
    "app.interrupt.dismiss": "Got it",
    "app.coding.appliedWithHead": "Applied to current branch · {head}",
    "app.coding.applied": "Applied to current branch",
    "app.coding.awaitingDelivery":
      "Changes are isolated · Waiting for your delivery decision",
    "app.coding.landingBlocked": "Pre-landing safety checks failed",
    "app.coding.error": "Error",
    "backend.ui.badLocale": "Invalid UI locale",
    "backend.agent.missingApiKey": "API key is required",
    "backend.agent.unknownAccess": "Unknown agent access mode: {access}",
    "backend.agent.notFound": "Unknown agent",
    "backend.agent.missingId": "agent_id is required",
    "backend.agent.invalidReasoningTier": "Invalid reasoning_tier: {tier}",
    "backend.agent.nativeAccessImmutable":
      "A native agent's access mode cannot be changed",
    "backend.agent.nativeKeyUnsupported":
      "API keys cannot be configured for native agents",
    "backend.agent.keychainSaveFailed":
      "Could not save the API key to the system keychain, so the key was not applied. Try again or check the system keychain permissions. (Details: {detail})",
    "backend.agent.keychainKeyUnavailable": "{detail}",
    "backend.agent.sessionRunUnknown":
      "This session has no run history, so its agent cannot be determined",
    "backend.agent.idNotFound": "Agent {id} does not exist",
    "backend.agent.emptyFilteredId": "The agent id is empty after filtering",
    "backend.agent.unknownEngine": "Unknown engine: {engine}",
    "backend.agent.configDirCreateFailed":
      "Failed to create the configuration directory: {detail}",
    "backend.agent.missingEndpoint": "Agent {id} is missing an endpoint",
    "backend.member.notInSessionPool":
      "Agent {id} is not in this session's member pool",
    "backend.member.unavailableMissing":
      "Agent {id} is unavailable because it does not exist",
    "backend.member.unavailableDisabled":
      "Agent {id} is unavailable because it is disabled",
    "backend.member.emptyTeam": "A team run requires at least one member",
    "backend.member.spawnFailed": "Failed to start the member: {detail}",
    "backend.member.noResult":
      "run_single_worker: the worker did not produce a MemberResult",
    "backend.gh.gitSpawnFailed": "Failed to start Git: {detail}",
    "backend.mcp.noPort": "Unable to obtain the listening port",
    "backend.proxy.noPort": "The proxy could not resolve its listening port",
    "backend.criteria.lineTooLong":
      "An acceptance criterion exceeds the {max}-character limit",
    "backend.criteria.invalidSyntax":
      "Invalid acceptance criterion: {raw} (use cmd:/contains:<s>:/judge:)",
    "backend.criteria.tooMany":
      "There are too many acceptance criteria (maximum {max})",
    "backend.file.markdownOnly": "Only .md/.markdown files are allowed",
    "backend.file.parentMissing": "The parent directory does not exist",
    "backend.file.pathOutOfBounds": "The path is outside the project",
    "backend.file.notFound": "The file does not exist",
    "backend.file.openFilesOnly": "Only files can be opened",
    "backend.file.tooLarge":
      "The file is {size} bytes; text preview supports up to {max} bytes",
    "backend.file.binaryPreviewUnsupported":
      "Binary file preview is not supported yet",
    "backend.file.htmlOnly": "Only .html/.htm files can be opened in a browser",
    "backend.file.ambiguousBasename":
      "Multiple files have this name. Use a more complete path: {0} → {1}",
    "backend.file.basenameBudget":
      'Too many files to search for "{0}" — please provide a fuller path',
    "backend.file.openExternalFailed":
      "Could not open the file in the system browser: {detail}",
    "backend.file.repoLookupFailed": "Failed to look up the project: {detail}",
    "backend.file.repoNotFound": "The project does not exist",
    "backend.repo.namespaceLookupFailed":
      "Failed to look up the namespace: {detail}",
    "backend.repo.lookupFailed": "Failed to look up the repository: {detail}",
    "backend.repo.activeReposLookupFailed":
      "Failed to list active repositories: {detail}",
    "backend.repo.duplicateLookupFailed":
      "Failed to check for an existing repository: {detail}",
    "backend.repo.setLastActiveFailed":
      "Failed to set the last active repository: {detail}",
    "backend.repo.ensureNamespaceFailed":
      "Failed to ensure the namespace exists: {detail}",
    "backend.repo.insertRepoFailed": "Failed to add the repository: {detail}",
    "backend.repo.namespaceMismatch":
      "Repository {repoId} belongs to {actualNamespaceId}, not {namespaceId}",
    "backend.repo.pathNotFound": "The path does not exist: {path}",
    "backend.repo.pathNotDirectory": "The path is not a directory: {path}",
    "backend.repo.pathInsideAppDomain":
      "Projects inside AgentLoom's own data directory (~/.agentloom) cannot be added: {path}. Move the project outside that directory and try again.",
    "backend.repo.insertFailed": "Failed to add the repository: {detail}",
    "backend.landing.protectedPath":
      "Pre-landing check failed: protected paths {paths}",
    "backend.landing.noEvidence":
      "Pre-landing check failed: worker changed_files evidence not found",
    "backend.landing.scopeExceeded":
      "Pre-landing check failed: changes exceed the worker declaration {files}",
    "backend.landing.l1NotGreen":
      "L1 is not green (no passed re-verification, or the evidence SHA does not match the current commit) · Merge blocked · See spec L4",
    "backend.merge.stagingConflict":
      "Changes conflict with staging · Rejected (conflicts are not resolved automatically)",
    "backend.finalize.noChanges":
      "The worker made no changes · Nothing to finalize",
    "backend.finalize.gitUnavailable":
      "This project is not a Git repository; the agent's changes remain in place, but Git relay is unavailable",
    "backend.finalize.uncommittedChanges":
      "The worker left uncommitted changes; the app will not commit them, and they remain in the working directory",
    "backend.artifact.notReadyVerify":
      "Artifact is not ready (no commit_sha) · Cannot verify",
    "backend.artifact.noShaPreflight":
      "Artifact has no commit_sha · Cannot run pre-landing checks",
    "backend.artifact.notReadyMerge":
      "Artifact is not ready (state={state}) · Cannot merge",
    "backend.artifact.noShaMerge": "Artifact has no commit_sha · Cannot merge",
    "backend.artifact.notFound": "Artifact does not exist: {id}",
    "backend.run.repoNotFound": "Repository {id} does not exist",
    "backend.run.preHeadReadFailed": "Failed to read pre_head: {detail}",
    "backend.run.ledgerPendingWriteFailed":
      "Failed to write the pending ledger entry: {detail}",
    "backend.run.spawnFailed": "Failed to start the run: {detail}",
    "backend.run.teamMembersActive": "Cannot start a new run: {detail}",
    "backend.run.stdoutUnavailable": "Unable to read run output",
    "backend.run.workspaceCanonicalizeFailed":
      "Failed to canonicalize the workspace: {detail}",
    "backend.run.unknownLeadAgent": "Unknown lead agent: {id}",
    "backend.run.unknownLeadAgentGeneric": "Unknown lead agent",
    "backend.run.tombstoneRestoreFailed":
      "Failed to tombstone the session and restore its branch (database/Git divergence; reconciliation required): tombstone={tombstone}; restore={restore}",
    "backend.run.invalidSessionId": "Invalid session_id",
    "backend.run.inplaceDeliveryUncommitted":
      "In-place changes are not committed yet: {count} file(s) remain in the working tree ({files}). Commit them in the project first, then push / create a PR / publish.",
    "backend.delivery.confirmationRequired":
      "{operation} requires explicit confirmation for this attempt; no remote operation was performed",
    "backend.publish.pushed": "Pushed to origin/{branch}",
    "backend.publish.failed.boundRepo":
      "PUBLISH_FAILED:This session is linked to a GitHub repository; use push/PR instead of publish",
    "backend.publish.needsAccount.missing":
      "PUBLISH_NEEDS_ACCOUNT:No signed-in gh account was detected (run gh auth login)",
    "backend.publish.needsAccount.multiple":
      "PUBLISH_NEEDS_ACCOUNT:Multiple gh accounts were detected; choose an identity ({list})",
    "backend.publish.failed": "PUBLISH_FAILED:{detail}",
    "backend.publish.failed.missingRepoName":
      "PUBLISH_FAILED:repo_name is required (no goal_title fallback is available)",
    "backend.continuation.invalidParentSessionId":
      "The parent session_id is empty after sanitization; cannot create a continuation child session",
    "backend.continuation.childSessionIdUnavailable":
      "Unable to generate a unique continuation child session id",
    "backend.continuation.startCleanupFailed":
      "{original}; cleanup errors: {errors}",
    "backend.continuation.handoffRequired":
      "The continuation launch instructions (handoff document) cannot be empty",
    "backend.continuation.handoffTimedOut":
      "Continuation draft generation timed out. Please retry; the parent session is available again.",
    "backend.continuation.invalidSessionId":
      "The session_id is empty after sanitization; cannot assemble the continuation handoff",
    "backend.continuation.localSessionUnsupported":
      "Local sessions do not support continuation yet (this feature is not yet available for local sessions)",
    "backend.lead.claudeOnlyContinuation":
      "Team continuation currently supports only native Claude sessions (non-Claude lead support is in progress; use Solo for now)",
    "backend.apply.repoDetached":
      "The current repository has a detached HEAD and is not on a branch, so changes were not applied (check out a branch first)",
    "backend.apply.repoDirty":
      "The current repository worktree has uncommitted changes; commit or stash them before applying",
    "backend.apply.branchAdvanced":
      "The current branch cannot fast-forward to staging (it may have advanced or diverged; v1 will not force-push): the current branch has advanced",
    "backend.apply.fastForwardFailed":
      "The current branch cannot fast-forward to staging (it may have advanced or diverged; v1 will not force-push): {detail}",
    "backend.wt.verifier.unsupportedPlatform":
      "The verifier sandbox is unavailable on this platform. The MVP supports macOS only; Linux sandboxing is deferred.",
    "backend.wt.verifier.writeAttempt":
      "The verifier attempted to modify files. The attempt was rejected; use dispatch_worker for file changes.",
    "backend.wt.verifier.canonicalizeFailed":
      "Could not canonicalize the session worktree / HOME path; the verifier refuses to run in-place without sandbox guardrails: {detail}",
    "backend.wt.sessionMerge.artifactBaseMismatch":
      "Artifact {artifact} is not based on base_sha {base}; merge rejected",
    "backend.wt.sessionMerge.stagingBaseMismatch":
      "Staging branch {staging} is not based on base_sha {base}; merge rejected",
    "backend.wt.sessionMerge.outsideAppDomain":
      "Stage 1 merge rejected: session_wt {path} is outside the app domain (~/.agentloom) · fail-closed",
    "backend.wt.sessionMerge.invalidHead":
      "Stage 1 merge rejected: the session worktree HEAD is not attached to agentloom/* (detached or on a non-agentloom branch) · fail-closed",
    "backend.wt.sessionMerge.memberMissing":
      "Stage 1 merge rejected: member branch {member} does not exist",
    "backend.wt.sessionMerge.dirtyWorktree":
      "Stage 1 merge rejected: the session worktree has uncommitted changes · fail-closed",
    "backend.wt.sessionMerge.stagingBranchMissing":
      "Staging branch does not exist: {staging}",
    "backend.wt.cleanup.commitOutsideAppDomain":
      "commit_dirty rejected: {path} is outside the app domain · fail-closed",
    "backend.wt.cleanup.commitInvalidHead":
      "commit_dirty rejected: {path} HEAD is not attached to agentloom/* · fail-closed",
    "backend.wt.cleanup.sessionWorktreeReleased":
      "Cleanup rejected: the session worktree was released while {pending} member branches still need merging · fail-closed (recreate the session worktree and retry, or reconcile)",
    "backend.wt.cleanup.invalidMemberRef":
      "Cleanup rejected: member ref {member} has an invalid format · fail-closed",
    "backend.wt.cleanup.memberWorktreeDetached":
      "Cleanup rejected: member worktree {path} is not attached to {member} (detached or invalid state) · fail-closed · reconcile required",
    "backend.wt.cleanup.notFastForward":
      "Cleanup rejected: member {member} cannot be fast-forwarded (stale base or parallel changes) · fail-closed",
    "backend.wt.cleanup.registrationIncomplete":
      "Release/trash rejected: worktree {path} is still registered · fail-closed",
    "backend.wt.cleanup.trashRefExists":
      "Trash rejected: {trash} already exists; refusing to overwrite the previous recovery copy · fail-closed",
    "backend.wt.restore.headsRefExists":
      "Restore rejected: {heads} already exists; refusing to overwrite the live branch · fail-closed",
    "backend.wt.restore.refsMissing":
      "Restore rejected: neither the trash nor heads ref exists for session {session}; nothing can be restored · fail-closed",
    "backend.wt.restore.compensationTrashExists":
      "Restore compensation rejected: {trash} already exists; refusing to overwrite the trash ref · fail-closed",
    "backend.wt.restore.compensationHeadsMissing":
      "Restore compensation rejected: {heads} does not exist and cannot be moved back to trash · fail-closed",
    "backend.wt.gc.liveWorktree":
      "GC rejected: session {session} still has a live registered worktree · fail-closed",
    "backend.wt.gc.liveHeads":
      "GC rejected: session {session} still has a live heads branch; its base is the diff fork point · fail-closed",
    "backend.wt.session.gitStatusSpawnFailed":
      "Failed to start git status: {detail}",
    "backend.wt.session.gitStatusFailed": "git status failed: {detail}",
    "backend.wt.session.worktreeDirty":
      "The worktree has uncommitted changes (the ledger expects an idle clean state)",
    "backend.wt.session.postHeadMissing":
      "Ledger post_head {postHead} does not exist in Git",
    "backend.wt.session.postHeadNotAncestor":
      "Ledger post_head {postHead} is not an ancestor of the current HEAD",
    "backend.wt.session.invalidDefaultId":
      "session_id is empty after sanitization; cannot create the default worktree",
    "backend.wt.session.invalidId":
      "session_id is empty after sanitization; cannot create the worktree",
    "backend.wt.session.invalidMemberIds":
      "session_id or assignment_id is empty after sanitization; cannot create the member worktree",
    "backend.wt.session.invalidSessionId": "Invalid session_id",
    "backend.wt.continuation.invalidIds":
      "The parent or child session_id is empty after sanitization; cannot derive the continuation worktree",
    "backend.wt.continuation.childBranchExists":
      "The continuation child branch already exists: {child}",
    "backend.wt.continuation.baseRefExists":
      "The continuation child base ref already exists: {base}",
    "backend.wt.continuation.invalidChildId":
      "The child session_id is empty after sanitization; cannot clean up the continuation worktree",
    "backend.wt.continuation.pathNotUtf8":
      "The worktree path is not valid UTF-8: {path}",
    "backend.wt.continuation.removeResidualFailed":
      "Failed to remove the leftover worktree directory: {detail}",
    "backend.wt.continuation.refsStillRegistered":
      "Continuation cleanup refused to delete refs because the child worktree is still registered: {path}",
    "backend.wt.git.spawnFailed": "Failed to start git {cmd}: {detail}",
    "backend.wt.git.commandFailed": "git {cmd} failed: {stderr}",
    "backend.wt.git.revParseSpawnFailed":
      "Failed to start git rev-parse: {detail}",
    "backend.wt.git.revParseFailed": "git rev-parse HEAD failed: {stderr}",
    "backend.wt.git.sessionStatusSpawnFailed":
      "Failed to inspect session_wt {phase}-status because git could not start: {detail}",
    "backend.wt.git.sessionStatusFailed":
      "Failed to inspect session_wt {phase}-status; git {cmd} failed: {stderr}",
    "backend.wt.git.verifierSpawnFailed":
      "Failed to start the verification command: {detail}",
    "backend.wt.git.worktreeListFailed":
      "Failed to run git worktree list: {detail}",
    "backend.wt.git.worktreeListNonZero":
      "git worktree list exited non-zero (code {exitCode}): {stderr}",
    "backend.wt.scaffold.worktreeAddSpawnFailed":
      "Failed to start git worktree add: {detail}",
    "backend.wt.scaffold.verifyCheckoutFailed":
      "Failed to create the temporary verification checkout: {stderr}",
    "backend.wt.scaffold.stagingWorktreeFailed":
      "Failed to create the staging worktree: {stderr}",
    "backend.wt.scaffold.createDirFailed":
      "Failed to create the directory: {detail}",
    "backend.wt.scaffold.defaultInitSpawnFailed":
      "Failed to start git init: {detail}",
    "backend.wt.scaffold.defaultInitFailed": "git init failed: {stderr}",
    "backend.wt.scaffold.sessionWorktreeSpawnFailed":
      "Failed to start git worktree: {detail}",
    "backend.wt.scaffold.sessionWorktreeFailed":
      "git worktree failed: {stderr}",
    "backend.wt.scaffold.continuationWorktreeSpawnFailed":
      "Failed to start the continuation git worktree: {detail}",
    "backend.wt.scaffold.continuationWorktreeFailed":
      "The continuation git worktree failed: {stderr}",
    "backend.wt.scaffold.memberWorktreeSpawnFailed":
      "Failed to start the member git worktree: {detail}",
    "backend.wt.scaffold.memberWorktreeFailed":
      "The member git worktree failed: {stderr}",
    "backend.db.restore.parentMissing":
      "The parent session does not exist, so the continuation child cannot be restored",
    "backend.db.restore.parentDeleted":
      "The parent session is deleted, so the continuation child cannot be restored",
    "backend.db.restore.parentPointsElsewhere":
      "The parent session's continued_to_session_id points to another child, so this continuation child cannot be restored",
    "backend.db.restore.liveChildExists":
      "The parent session already has a live child, so the old continuation child cannot be restored",
    "backend.db.memory.badJson": "{field} is not valid JSON: {detail}",
    "backend.lead.spawnDriverFailed": "Failed to spawn driver: {detail}",
    "backend.lead.spawnLeadFailed": "Failed to spawn lead: {detail}",
    "backend.lead.noFinalText": "Lead produced no terminal final_text",
    "backend.lead.noFinalTextStderr":
      "Lead produced no terminal final_text · stderr tail: {stderr}",
    "backend.lead.claudeOnlyBlock1":
      "Block 1 only supports a native claude Lead (current provider={provider} access={access})",
    "backend.lead.engineNotSupported":
      "This engine can't be a lead yet (provider={provider} access={access})",
    "backend.team.oneshotSpawnFailed":
      "Failed to start run_oneshot_llm: {detail}",
    "backend.team.oneshotFailed": "run_oneshot_llm failed: {detail}",
    "backend.team.oneshotNoText": "run_oneshot_llm produced no assistant text",
    "backend.team.noMemberOutput":
      "No team member produced output, so synthesis cannot continue",
    "backend.team.summarizeSpawnFailed":
      "Failed to start lead_summarize: {detail}",
    "backend.team.summarizeFailed": "Lead synthesis failed: {detail}",
    "backend.team.summarizeNoText": "Lead synthesis produced no assistant text",
    "backend.lead.claudeOnlyStep":
      "lead_step only supports a native claude Lead (current access={access} provider={provider})",
    "backend.lead.parseSpawnFailed": "Unable to parse Lead output: {detail}",
    "backend.lead.parseNoOutput": "Unable to parse Lead output: {detail}",
    "backend.lead.parseFailed": "Unable to parse Lead output: {detail}",
    "backend.lead.claudeOnlyDraft":
      "B1 only supports native claude as the driver (current access={access} provider={provider}; borrowed/codex drivers are deferred)",
    "backend.lead.draftNoFinalText":
      "Driver produced no terminal final_text or reported an error",
    "backend.lead.draftNoFinalTextStderr":
      "Driver produced no terminal final_text or reported an error · stderr tail: {tail}",
    "backend.leadTools.askUserNeedsOptions":
      "ask_user requires options (at least 2)",
    "backend.leadTools.verifierLocalUnsupported":
      "propose_verifier: Local sessions are not supported yet",
    "app.coding.awaitingDeliveryNarration":
      "Done. The changes are isolated—choose whether to commit, push, or open a PR below.",
    "app.session.new": "New session",
    "app.dispatch.noAvailableMembers": "This Team has no available members.",
    "app.dispatch.hintNotFound":
      "agent_hint “{hint}” is not in the dispatchable member pool. Not dispatching.",
    "app.dispatch.hintAmbiguous":
      "agent_hint “{hint}” matches multiple dispatchable members. Not dispatching.",
    "app.dispatch.hintRequired":
      "More than one member is dispatchable, but the Lead did not provide agent_hint. The Lead must choose a worker.",
    "app.dispatch.scopeSuggestion": "{task}\n(Suggested scope: {scope})",
    "app.listSeparator": ", ",
    "app.dispatch.question": "Dispatch to {name}: {task}",
    "app.verify.noActiveChanges":
      "I want to verify with “{command}”, but there are no active changes. Should I dispatch a worker first?",
    "app.verify.dispatchFirst": "Dispatch a worker",
    "app.verify.later": "Leave it for now",
    "app.delivery.noChanges": "{rationale} · No changes ready for delivery",
    "app.delivery.applied": "{rationale} · Applied to current branch ({head})",
    "app.delivery.prCreated": "{rationale} · PR opened: {url}",
    "app.delivery.published": "{rationale} · Published: {url}",
    "app.delivery.confirmPush":
      "Push the current branch to the remote? Undo cannot reverse this operation.",
    "app.delivery.confirmCreatePr":
      "Push the current branch and create a pull request? Undo cannot reverse these remote operations.",
    "app.delivery.confirmPublish":
      "Create a remote repository and publish this project? Undo cannot reverse this operation.",
    "app.lead.transientQuestion":
      "The Lead didn't respond. It may be temporarily unavailable—could you rephrase that?",
    "app.lead.genericQuestion":
      "The Lead couldn't decide what to do next. Could you rephrase that?",
    "app.lead.retry": "Let me rephrase",
    "app.lead.transientRationale": "Lead temporarily unavailable",
    "app.lead.genericRationale": "Lead step failed",
    "app.run.error": "\n[Error] {message}",
    "app.run.stopped": "\n[Stopped] {message}",
    "app.repo.alreadyAdded": "Already in the list · Switched to this project",
    "app.session.moveRepoMismatch":
      "This session does not belong to this repository and cannot be moved here",
    "app.run.startFailed": "[Failed to start] {error}",
    "app.session.deleteBody":
      "The session will be removed from the list and permanently deleted after 30 days.",
    "app.session.deleteBodyWithContinuations":
      "This session has {count} active continuations. Only this session will be deleted; its continuations will remain as independent sessions. This session will be removed from the list and permanently deleted after 30 days.",
    "app.gate.goalRequired": "Enter a goal before freezing it.",
    "app.gate.unassigned":
      "Some subtasks have no agent. Choose one under Assignment before starting.",
    "app.header.sessionFallback": "Session",
    "app.header.overviewContext": "Overview · {name}",
    "app.header.introContext": "{name} · Project overview",
    "app.project.archived": "Project archived",
    "app.project.restored": "Project restored to active",
    "app.project.switchedDefault": "Switched to the default session",
    "app.session.deleteTitle": "Delete session “{title}”?",
    "app.dialog.delete": "Delete",
    "app.group.deleteTitle": "Delete group “{name}”?",
    "app.group.deleteBody":
      "Sessions in this group will move to Ungrouped. No sessions will be deleted.",
    "repoSwitcher.searchPlaceholder": "Search projects or owners…",
    "repoSwitcher.empty": "No projects yet · Create or connect one below",
    "repoSwitcher.projectsGroup": "Projects",
    "repoSwitcher.newProject": "New project",
    "repoSwitcher.editProject": "Edit project",
    "repoSwitcher.manageRepos": "Manage GitHub repos",
    "newProject.title": "New project",
    "newProject.editTitle": "Edit project",
    "newProject.hint":
      "A project is the home for all the conversations and files about one thing.",
    "newProject.name.label": "Name",
    "newProject.name.placeholder": "Project name",
    "newProject.location.label": "Location",
    "newProject.location.newFolder": "New folder",
    "newProject.location.existingFolder": "Choose existing folder",
    "newProject.location.existingHint":
      "Make an existing folder a project without moving or changing its files.",
    "newProject.location.willCreate": "Will create {path}",
    "newProject.identity.label": "Icon (emoji)",
    "newProject.identity.option": "Project icon {emoji}",
    "newProject.cta.cancel": "Cancel",
    "newProject.cta.create": "Create project",
    "newProject.cta.save": "Save",
    "newProject.cta.remove": "Remove project",
    "removeProject.confirm.title": "Remove project?",
    "removeProject.confirm.body":
      'Remove "{name}"? Its {count} session(s) will be hidden together (data kept, disk code untouched). Restore from Settings › Archived projects.',
    "removeProject.confirm.confirm": "Remove",
    "removeProject.confirm.cancel": "Cancel",
    "namespaceDropdown.searchPlaceholder": "Search namespaces…",
    "namespaceDropdown.builtin": "Built in",
    "namespaceDropdown.cannotDelete": "Cannot delete",
    "namespaceDropdown.manageRepos": "Manage repositories",
    "namespaceDropdown.connectGithub": "Connect GitHub repo",
    "repoDropdown.searchPlaceholder": "Search repositories…",
    "projectSwitcher.selectProject": "Select project",
    "projectSwitcher.myProject": "My Project",
    "projectSwitcher.ariaLabel": "Project switcher",
    "projectSwitcher.settings": "Settings",
    "projectSwitcher.sessionCount": "{n} sessions",
    "invalidProjectDialog.title.invalid": "Project path is no longer valid",
    "invalidProjectDialog.title.archived": "Project archived",
    "invalidProjectDialog.body.invalid":
      "The project's original path could not be found. It may have been moved or deleted. Archive the project to keep its session history, or switch to the default session.",
    "invalidProjectDialog.body.archived":
      "This project is archived and hidden from daily use. Restore it as an active project, or switch to the default session.",
    "invalidProjectDialog.switchDefault": "Go to default session",
    "invalidProjectDialog.archive": "Archive project",
    "invalidProjectDialog.restore": "Restore",
    "confirmDialog.cancel": "Cancel",
    "cloneProgress.openSession": "Open session",
    "cloneProgress.cloning": "Cloning…",
    "cloneProgress.occupied": "Destination already exists",
    "cloneProgress.retry": "Retry",
    "cloneProgress.nonBlocking":
      "Non-blocking: Open sessions as soon as cloning finishes. Retry failures in place without affecting other repositories.",
    "scrollButtons.top": "Scroll to top",
    "scrollButtons.bottom": "Scroll to bottom",
    "repoConnection.error.notGit": "This is not a Git repository.",
    "repoConnection.error.notGithub":
      "This is not a GitHub repo. Use Local to add a local project.",
    "repoConnection.error.noCommits":
      "This repo has no commits yet and is not supported.",
    "repoConnection.error.alreadyAdded":
      "This repo is already added. Switched to it.",
    "repoConnection.error.generic": "Connection failed. Try again.",
    "repoManage.error.offline":
      "No network connection. Local sessions and cloned repositories are still available; GitHub discovery and cloning are unavailable.",
    "repoManage.error.timeout":
      "Reading GitHub repositories timed out. Check your connection and try again.",
    "repoManage.error.accountTimeout":
      "Reading GitHub accounts timed out. Check your system credentials or try again later.",
    "repoManage.error.authExpired":
      "Authorization for @{login} has expired. Sign in again.",
    "repoManage.error.loadFailed": "Failed to load repositories",
    "repoManage.gate.missing.title": "GitHub CLI (gh) required",
    "repoManage.gate.missing.description":
      "GitHub CLI is required to list and clone remote repositories.",
    "repoManage.gate.checking.title": "Checking repository environment…",
    "repoManage.gate.checking.description":
      "Checking Git, GitHub CLI, and account status.",
    "repoManage.gate.missingGit.title": "Git required",
    "repoManage.gate.missingGit.description":
      "AgentLoom uses Git to manage workspaces and changes.",
    "repoManage.gate.missingGit.install": "Install Git",
    "repoManage.gate.accountError.title": "Failed to read GitHub accounts",
    "repoManage.gate.installing": "Installing…",
    "repoManage.gate.install": "Install gh",
    "repoManage.gate.manualInstall": "Manual installation guide",
    "repoManage.gate.connect.title": "Connect a GitHub account",
    "repoManage.gate.connect.instructions.prefix": "Run",
    "repoManage.gate.connect.instructions.suffix":
      " in your terminal, then refresh.",
    "repoManage.refresh": "Refresh",
    "repoManage.recheck": "Check again",
    "repoManage.title": "Repositories · Manage",
    "repoManage.subtitle": "Discover by account → clone as needed",
    "repoManage.switchAccount": "Switch account",
    "repoManage.checkingAria": "Checking repository environment",
    "repoManage.checking": "Checking environment…",
    "repoManage.status.missingGit": "Git not installed",
    "repoManage.status.missingGh": "gh not installed",
    "repoManage.status.accountError": "Failed to read accounts",
    "repoManage.status.noAccount": "No GitHub account connected",
    "repoManage.loadingAria": "Reading repositories",
    "repoManage.loading": "Reading repositories…",
    "repoManage.idle": "Remote repositories have not been read yet",
    "repoManage.read": "Read",
    "repoManage.counts": "{cloned} cloned / {remote} remote",
    "repoManage.retry": "Retry",
    "repoManage.selection.summaryPrefix": "Selected",
    "repoManage.selection.summarySuffix": "· save to",
    "repoManage.selection.destinationHint":
      "(organized by github.com/org/repo)",
    "repoManage.selection.identity": "Commit as @{login}",
    "repoManage.clone": "Clone",
    "repoManage.connectLocal": "Add a locally cloned repo",
    "repoManage.connectAction": "Connect GitHub account",
    "repoManage.connect.instructions.prefix": "Run",
    "repoManage.connect.instructions.suffix":
      " in your terminal to add an account, then refresh.",
    "repoList.status.occupied": "Destination already exists",
    "dispatchCard.goalFallback": "Current task",
    "teamRun.errorPrefix": "Error: ",
    "repoList.search.placeholder": "Search repositories…",
    "repoList.filter.all": "All",
    "repoList.filter.cloned.first": "Clone",
    "repoList.filter.cloned.second": "d",
    "repoList.filter.remote.first": "Re",
    "repoList.filter.remote.second": "mote",
    "repoList.group.batch": "Cloning · current batch",
    "repoList.status.cloningAria": "Cloning",
    "repoList.empty": "Empty repository",
    "repoList.status.cloning": "Cloning…",
    "repoList.openSession": "Open session",
    "repoList.status.cloneFailed": "Clone failed",
    "repoList.retry": "Retry",
    "repoList.group.cloned": "Cloned · available locally",
    "repoList.group.remote": "Remote · not cloned",
    "repoList.group.remoteHint": " · select multiple to clone",
    "repoList.updatedAt": "· Updated {value}",
  },
} as const;

export type I18nKey = keyof typeof messages.zh;
export type TranslationKey = I18nKey;
export type TFn = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

type I18nContextValue = {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  t: TFn;
};

function normalizeLocale(value: string | null | undefined): Locale | null {
  if (!value) return null;
  const lower = value.toLowerCase();
  if (lower.startsWith("zh")) return "zh";
  if (lower.startsWith("en")) return "en";
  return null;
}

function detectLocale(): Locale {
  try {
    const stored = normalizeLocale(window.localStorage?.getItem(STORAGE_KEY));
    if (stored) return stored;
  } catch {
    // localStorage can be unavailable in tests or hardened webviews.
  }

  try {
    const systemLanguage =
      typeof navigator === "undefined"
        ? undefined
        : navigator.language || navigator.languages?.[0];
    if (normalizeLocale(systemLanguage) === "zh") return "zh";
  } catch {
    // Navigator language APIs can be unavailable in hardened webviews.
  }

  // First launch follows the system language, with English as the safe fallback.
  return "en";
}

function translate(
  locale: Locale,
  key: I18nKey,
  values?: Record<string, string | number>,
): string {
  let template: string = messages[locale][key] ?? messages.zh[key] ?? key;
  if (!values) return template;
  for (const [name, value] of Object.entries(values)) {
    template = template.split(`{${name}}`).join(String(value));
  }
  return template;
}

const defaultValue: I18nContextValue = {
  locale: "zh",
  setLocale: () => {},
  t: (key, values) => translate("zh", key, values),
};

const I18nContext = createContext<I18nContextValue>(defaultValue);

export function I18nProvider({
  children,
  initialLocale,
}: {
  children: ReactNode;
  initialLocale?: Locale;
}) {
  const [locale, setLocaleState] = useState<Locale>(
    () => initialLocale ?? detectLocale(),
  );

  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    try {
      void invoke("set_ui_locale", { locale }).catch(() => {});
    } catch {
      // Keep initialization and switching functional outside the Tauri runtime.
    }
  }, [locale]);

  const setLocale = useCallback((next: Locale) => {
    setLocaleState(next);
    try {
      window.localStorage?.setItem(STORAGE_KEY, next);
    } catch {
      // Keep language switching functional even when persistence is unavailable.
    }
  }, []);

  const value = useMemo<I18nContextValue>(
    () => ({
      locale,
      setLocale,
      t: (key, values) => translate(locale, key, values),
    }),
    [locale, setLocale],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  return useContext(I18nContext);
}

export const DEFAULT_LOCAL_PROJECT_ID = "local-default";
export const DEFAULT_LOCAL_PROJECT_NAME_SENTINEL = "我的项目";

export function localProjectDisplayName(
  repo: Pick<RepoMeta, "id" | "name">,
  t: I18nContextValue["t"],
): string {
  return repo.id === DEFAULT_LOCAL_PROJECT_ID &&
    repo.name === DEFAULT_LOCAL_PROJECT_NAME_SENTINEL
    ? t("projectSwitcher.myProject")
    : repo.name;
}
