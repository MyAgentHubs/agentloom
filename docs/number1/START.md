# 给新贡献者的 60 秒入口

1. 先读仓库 AGENTS.md / CONTRIBUTING.md，再读本目录 README.md 的目标、当前状态与发布前置项。
2. 查看 state/publication.json：approved 必须为 true、campaign_base_sha 必须是已发布真实 SHA。否则只审阅，不开工。
3. 先读 [MODEL-POLICY.md](MODEL-POLICY.md)，确认实际模型/effort 和共享预算；缺模型先确认替代映射。读取 state/index.json，领取一个依赖完成的节点；保持一个 contributor 一个 PR。
4. 用 `python3 docs/number1/tools/check_graph.py --node <ID>` 只取该节点合同。不要每轮把整张 graph 和旧会话塞入上下文。
5. 优先 REC-L2B6 / REC-C1 复用已有补丁；随后 FIRST-TASKS.md 的 B2/C2 小子单。
6. file_epic 先按 steps 写精确 scope 的子单；子单各轮与各角色共用总预算，达到预算即保存状态停止。
7. EXECUTION.md 是 harness 和回归陷阱入口；跑命令必须记录真实 exit code。
8. 没有 macOS、远端源码或模型，不假装通过；选可执行节点或 escalate。无 Claude 依赖。
9. 只提交当前节点精确文件和证据。公共分支完工不代表外部仓任务完成。
