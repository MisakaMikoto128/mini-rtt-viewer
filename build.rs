fn main() {
    slint_build::compile_with_config(
        "src/ui/app.slint",
        slint_build::CompilerConfiguration::new()
            // 自定义样式目录:只放覆盖的 fluent 文件,编译器优先从这找,
            // 未覆盖的文件(如 lineedit.slint)回落到内置样式库。
            // 现覆盖 styling.slint:浅色下控件边框(FluentPalette.control-border
            // 的黑色渐变)压平为极淡单色,"一圈灰边"观感消除
            .with_include_paths(vec![std::path::PathBuf::from("src/ui/style-override")]),
    )
    .unwrap();
}
