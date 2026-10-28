## 改动说明

<!-- 改了什么,为什么需要?关联 issue 请写 Closes #编号。 -->

## 验证

<!-- 勾选实际执行过的项目,不要勾选未执行的验证。 -->

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `python -m pytest tests/web -q`
- [ ] 真实 J-Link 验证(请注明芯片、接口和验证操作;未做请删除此项)
- [ ] 不涉及硬件行为,QA/单元测试已覆盖相关路径

## 界面与文档

- [ ] 行为变化已同步 `docs/product/spec.md`、README/CHANGELOG(如适用)
- [ ] UI 变化附截图,并保留左右布局与键盘可访问性(如适用)
- [ ] 截图和日志已遮蔽序列号、本机路径、个人信息与私有内容

## 补充说明

<!-- 兼容性、已知限制或审查者需要重点看的部分。 -->
