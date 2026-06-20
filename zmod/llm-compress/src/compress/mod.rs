//! 各内容类型压缩器集合。每个子模块实现一个 router::Compressor。
//! 03 truncate(兜底文本);04 json;05 diff;06 log —— 由各自任务追加 pub mod 行。

pub mod truncate;
pub mod json;
pub mod diff;
pub mod log;
