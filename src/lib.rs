//! 库 crate:所有业务模块的唯一编译处。binary 与 examples 一律 `use`
//! 本 crate,**不要**在 bin/main 里重复声明 mod 树——否则同一模块编译两份,
//! 而且容易漏声明(踩过:bin 里漏 `mod ansi` 编译失败的坑)。
//!
//! 两种形态共用本 crate:
//! - 桌面壳 `gui`(tao 事件循环 + wry WebView + 托盘,SerialHub 同构);
//! - 浏览器管理台服务 `web`(axum + 内嵌单页,`--no-window` 纯服务 / 壳内后台
//!   线程两种装配,见 web.rs 模块头)。

pub mod ansi;
pub mod config;
pub mod demo;
pub mod device_db;
pub mod gui;
pub mod jlink_dll;
pub mod log_model;
pub mod rtt;
pub mod web;
