# 贡献指南

感谢你愿意参与 AetherOS。这个项目最看重两件事：**可验证**与**诚实的边界**。

## 先读

- [文档导航](docs/README.md)
- [代码索引](docs/代码索引（INDEX）-2026-09-11.md)
- [路线图与已知缺口](docs/路线图与已知缺口（ROADMAP）-2026-09-05.md)
- [安全策略](SECURITY.md)

## 开发环境

- Rust workspace：`cargo test --workspace --offline --locked`
- 快速门禁：`python scripts/gates.py --fast`
- 完整门禁：`python scripts/gates.py`（需要构建产物、像素基线等）
- 文档检查：`python scripts/doc-numbers-check.py`、`python scripts/check-links.py`

Windows 开发机建议使用仓库约定的 GNU 工具链；Linux 侧请至少跑一次
`cargo check --target x86_64-unknown-linux-musl --all-targets`（`aetherd` 的 ring 交叉编译限制见文档）。

## 提交前检查

1. 代码改动必须补测试；没有测试的改动不会被当成“已完成”。
2. 涉及渲染的改动必须过 `python scripts/shot-diff.py verify`，像素差异必须为 0 或有明确批准。
3. 涉及文档数字的改动只改权威来源，再跑 `python scripts/doc-numbers-check.py`；
   不要手抄行数/测试数。
4. 新增本地链接必须能被 `python scripts/check-links.py` 解析。
5. 安全相关改动必须说明威胁模型、负向用例和回滚方式。

## 提交信息

使用中文或英文均可，但必须说明「改了什么、为什么、怎么验证」。建议格式：

```text
模块：一句话结论

- 根因/背景
- 改动点
- 验证命令与结果
- 未验证项（如有）
```

## 代码风格

- 保持现有模块边界与命名，不引入新的框架/范式。
- 生产代码不要留 `unwrap()` 作为错误处理；测试中的 `unwrap()` 需能说明“不可能失败”。
- 不提交密钥、口令、Token、私钥、VPS 凭据。
- 不提交 `target/`、ISO 构建中间产物或临时日志。

## 安全漏洞

不要开公开 Issue。按 [SECURITY.md](SECURITY.md) 的私密通道报告。

## 行为准则

参与本项目即表示同意 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。
