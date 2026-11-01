## 改了什么

<!-- 一句话结论，再列关键文件/模块。 -->

## 为什么

<!-- 根因、用户场景或要补的缺口。 -->

## 怎么验证

<!-- 实际跑过的命令与结果；没有验证的项明确写“未验证”。 -->

- [ ] `cargo test --workspace --offline --locked`
- [ ] `python scripts/gates.py --fast`
- [ ] 涉及渲染：`python scripts/shot-diff.py verify`
- [ ] 涉及文档：`python scripts/doc-numbers-check.py` 与 `python scripts/check-links.py`
- [ ] 没有提交密钥、密码、Token、私钥或生产凭据

## 风险与回滚

<!-- 影响范围、兼容性、如何回滚。安全相关请说明威胁模型与负向用例。 -->
