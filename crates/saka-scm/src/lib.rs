//! Source control adapters, GitHub first.

pub mod github;

pub use github::{GitHub, GitHubApp, GitHubConfig, RepoInfo};
