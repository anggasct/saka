use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use saka_core::vocab::Tool;
use saka_ports::{PortError, PortResult, Release, ReleaseIndex};
use serde_json::json;

#[derive(Default)]
pub struct FakeReleases {
    releases: BTreeMap<Tool, Vec<Release>>,
    failing: BTreeSet<Tool>,
    calls: Mutex<Vec<Tool>>,
}

impl FakeReleases {
    pub fn new() -> FakeReleases {
        FakeReleases::default()
    }

    pub fn with(mut self, tool: Tool, versions: &[&str]) -> FakeReleases {
        self.releases
            .entry(tool)
            .or_default()
            .extend(versions.iter().map(|v| Release {
                version: (*v).to_string(),
                lts: None,
            }));
        self
    }

    pub fn with_lts(mut self, tool: Tool, version: &str, lts: &str) -> FakeReleases {
        self.releases.entry(tool).or_default().push(Release {
            version: version.to_string(),
            lts: Some(lts.to_string()),
        });
        self
    }

    pub fn failing(mut self, tool: Tool) -> FakeReleases {
        self.failing.insert(tool);
        self
    }

    pub fn calls(&self) -> Vec<Tool> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ReleaseIndex for FakeReleases {
    async fn releases(&self, tool: Tool) -> PortResult<Vec<Release>> {
        self.calls.lock().unwrap().push(tool);
        if self.failing.contains(&tool) {
            return Err(PortError::retryable(
                "release_index_unavailable",
                json!({ "tool": tool, "detail": "HTTP 500" }),
            ));
        }
        Ok(self.releases.get(&tool).cloned().unwrap_or_default())
    }
}
