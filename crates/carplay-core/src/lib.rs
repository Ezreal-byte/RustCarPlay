// SPDX-License-Identifier: GPL-3.0-only
//! Platform-independent receiver policies and bounded CarPlay wire parsers.
pub mod config;
pub mod input;
pub mod media;
pub mod rtsp;
pub mod session;
pub mod vehicle;

pub const UPSTREAM_COMMIT: &str = "9e244d958afe6b8fd79ade49769ce25a944f397b";

/// Wire compatibility baseline used by DiPlay 9e244d9.
pub const SOURCE_VERSION: &str = "950.7.1";
