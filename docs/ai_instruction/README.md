# AI 指令与汇报材料（`ai_instruction`）

本目录存放 **面向 AI 辅助生成** 的说明文档，以及 **竞标/立项风格** 的产品报告底稿，内容与仓库主文档对齐：

- 根目录：`README.md`、`DEPLOYMENT_README.md`
- 上下文交接：`Context_Handoff.md`

## 文件一览

| 文件 | 用途 |
|------|------|
| [product-report.md](./product-report.md) | **产品报告**（竞标/汇报体，可单独交付或再压缩为 PPT 口播稿） |
| [kimi-ppt-prompt.md](./kimi-ppt-prompt.md) | **发给 Kimi 的一键指令**：复制全文即可生成 PPT 大纲与逐页文案 |
| [kimi-ppt-slide-spec.md](./kimi-ppt-slide-spec.md) | **逐页规格**：建议页序、每页必含元素、术语表、配图清单（供人工或 Kimi 精修） |
| [sag-bidding-deck.marp.md](./sag-bidding-deck.marp.md) | **可直接导出 PPT 的幻灯片源**（Marp），不依赖 Kimi |

## 不用 Kimi，如何得到 `.pptx`

1. 在 VS Code 安装扩展 **Marp for VS Code**（`marp-team.marp-vscode`）。
2. 打开 [`sag-bidding-deck.marp.md`](./sag-bidding-deck.marp.md)。
3. 右上角 **Open Preview**，确认分页正常。
4. 命令面板执行 **Marp: Export Slide Deck** → 选择 **PowerPoint (.pptx)**。
5. 在封面页把 `{客户名称}`、`{汇报日期}`、`{汇报人/部门}` 替换为实际内容后再导出（或导出后在 PowerPoint 里改）。

若无 VS Code：可将同一文件上传到 [Marp Web](https://web.marp.app/)（注意脱敏）或使用其它支持 Marp 的工具导出。

## 使用建议

1. **写 Word / 正式报告**：以 `product-report.md` 为主干，按甲方模板删节。
2. **生成 PPT**：先将 `kimi-ppt-prompt.md` 整段发给 Kimi；若页数或风格不满意，用 `kimi-ppt-slide-spec.md` 补充约束后再次生成。
3. **与研发口径一致**：涉及端口、路径、验收探针（N1/T1）等，以 `DEPLOYMENT_README.md` 与 `Context_Handoff.md` 为准；本目录在歧义时 **以后者为准**。

## 维护

更新产品能力或部署方式后，请同步修订 `product-report.md` 中与事实相关的段落，并在 `Context_Handoff.md` 中保留会话级变更记录。
