//! 库 crate:所有业务模块的唯一编译处。Slint 生成的 UI 代码(AppWindow、
//! LogRow 等)也在这里 include 并对外导出,binary 与 examples 一律 `use`
//! 本 crate,**不要**在 bin 里重复声明 mod 树——否则同一模块编译两份,
//! 而且容易漏声明(踩过:bin 里漏 `mod ansi` 编译失败的坑)。

pub mod ansi;
pub mod config;
pub mod demo;
pub mod device_db;
pub mod jlink_dll;
pub mod log_model;
pub mod rtt;
pub mod single_instance;

// Slint 编译产物(build.rs:src/ui/app.slint)在此展开,
// AppWindow / LogRow / LogRun 等生成类型经本 crate 对外可见。
slint::include_modules!();
