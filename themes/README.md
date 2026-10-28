# 自定义主题

在 `mini-rtt-viewer.exe` 旁创建 `themes/` 目录，放入一个或多个 `.css` 文件，然后刷新管理台，
在左栏「主题」下拉中选择。文件名（不含扩展名）即主题 ID；七套内置主题优先，不能被同名文件覆盖。

主题只需覆盖 `:root` 中的设计令牌，不需要修改程序或重新构建：

```css
:root {
  --bg: #101418;
  --panel: #171d22;
  --input: #11171b;
  --text: #dce5ea;
  --secondary: #9aabb5;
  --muted: #74858f;
  --border: #2d3941;
  --accent: #52a8d8;
  --accent-fg: #ffffff;
  --btn: #202a31;
  --log-bg: #0d1215;
  --log-fg: #dce5ea;
  --ok: #4caf7d;
  --err: #df6b6b;
}
```

完整令牌以 `ui/web/styles.css` 为准。CSS 只影响颜色、字体和尺寸等样式；不要导入远程资源，
也不要在主题中放入隐私信息。发布自定义主题时建议同时提供浅色/深色截图和 MIT 兼容许可说明。
