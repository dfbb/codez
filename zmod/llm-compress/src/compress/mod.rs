//! Collection of compressors for various content types. Each submodule implements a router::Compressor.
//! 03 truncate(fallback text); 04 json; 05 diff; 06 log — append pub mod lines as per their respective tasks.

pub mod truncate;
pub mod json;
pub mod diff;
pub mod log;
pub mod search;
pub mod tabular;
pub mod toon;
