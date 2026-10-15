#![no_std]

// Use the native p3 backend for dependencies that require getrandom 0.3.
pub use getrandom_04::{fill, fill_uninit, u32, u64, Error};
