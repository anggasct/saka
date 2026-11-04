use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use saka_core::authorship::Identity;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::AuthorizationFailure;
use saka_ports::{
    Clock, DeviceCode, DevicePoll, IdentityLookup, PortError, PortResult, TokenRefresh, UserTokens,
};
use serde_json::json;

use crate::clock::FakeClock;

pub const DEVICE_VERIFICATION_URI: &str = "https://github.com/login/device";
pub const ACCESS_TOKEN_SECS: u64 = 8 * 3600;
pub const REFRESH_TOKEN_SECS: u64 = 184 * 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAnswer {
    Pending,
    SlowDown(Option<u64>),
    Approve,
    Deny,
    Expire,
    Fail(AuthorizationFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCall {
    pub op: &'static str,
    pub credential: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
    pub name: Option<String>,
}

#[derive(Debug)]
struct Device {
    device_code: String,
    answers: VecDeque<DeviceAnswer>,
    spent: bool,
}

#[derive(Debug)]
struct State {
    device_flow: bool,
    interval_secs: u64,
    expires_in_secs: u64,
    next: u32,
    devices: Vec<Device>,
    user: GitHubUser,
    access_secs: Option<u64>,
    refresh_secs: Option<u64>,
    live_refresh: BTreeSet<String>,
    refresh_outages: u32,
    identity_rejected: bool,
    calls: Vec<AccountCall>,
    issued: Vec<String>,
}

// The user-account side of a fake GitHub: device authorizations a test approves, denies, slows
// down, or lets expire, single-use refresh tokens that rotate, and a log of the credential
// each call carried.
#[derive(Debug)]
pub struct FakeAccounts {
    clock: Arc<FakeClock>,
    state: Mutex<State>,
}

impl FakeAccounts {
    pub fn new(clock: Arc<FakeClock>) -> FakeAccounts {
        let state = Mutex::new(State {
            device_flow: true,
            interval_secs: 5,
            expires_in_secs: 900,
            next: 0,
            devices: Vec::new(),
            user: GitHubUser {
                id: 1,
                login: "octo".into(),
                name: Some("Octo Cat".into()),
            },
            access_secs: Some(ACCESS_TOKEN_SECS),
            refresh_secs: Some(REFRESH_TOKEN_SECS),
            live_refresh: BTreeSet::new(),
            refresh_outages: 0,
            identity_rejected: false,
            calls: Vec::new(),
            issued: Vec::new(),
        });
        FakeAccounts { clock, state }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn record(&self, s: &mut State, op: &'static str, credential: &str) {
        s.calls.push(AccountCall {
            op,
            credential: credential.to_string(),
            at: self.clock.now(),
        });
    }

    pub fn set_device_flow(&self, configured: bool) {
        self.state().device_flow = configured;
    }

    pub fn set_device_timing(&self, interval_secs: u64, expires_in_secs: u64) {
        let mut s = self.state();
        s.interval_secs = interval_secs;
        s.expires_in_secs = expires_in_secs;
    }

    pub fn set_user(&self, id: u64, login: &str, name: Option<&str>) {
        self.state().user = GitHubUser {
            id,
            login: login.into(),
            name: name.map(String::from),
        };
    }

    pub fn set_token_lifetimes(&self, access_secs: Option<u64>, refresh_secs: Option<u64>) {
        let mut s = self.state();
        s.access_secs = access_secs;
        s.refresh_secs = refresh_secs;
    }

    pub fn answer(&self, answer: DeviceAnswer) {
        if let Some(device) = self.state().devices.last_mut() {
            device.answers.push_back(answer);
        }
    }

    pub fn reject_identity(&self, rejected: bool) {
        self.state().identity_rejected = rejected;
    }

    pub fn fail_refreshes(&self, times: u32) {
        self.state().refresh_outages = times;
    }

    pub fn revoke_refresh_tokens(&self) {
        self.state().live_refresh.clear();
    }

    pub fn calls(&self) -> Vec<AccountCall> {
        self.state().calls.clone()
    }

    pub fn calls_of(&self, op: &str) -> Vec<AccountCall> {
        self.calls().into_iter().filter(|c| c.op == op).collect()
    }

    pub fn device_codes(&self) -> Vec<String> {
        self.state()
            .devices
            .iter()
            .map(|d| d.device_code.clone())
            .collect()
    }

    pub fn issued_tokens(&self) -> Vec<String> {
        self.state().issued.clone()
    }

    pub(crate) fn configured(&self) -> bool {
        self.state().device_flow
    }

    pub(crate) fn device_code(&self) -> PortResult<DeviceCode> {
        let mut s = self.state();
        if !s.device_flow {
            return Err(PortError::permanent(
                "device_flow_unconfigured",
                json!({ "integration_id": "github" }),
            ));
        }
        s.next += 1;
        let n = s.next;
        let device_code = format!("fakedevicecode{n:04}x9q");
        self.record(&mut s, "device_code", "");
        s.devices.push(Device {
            device_code: device_code.clone(),
            answers: VecDeque::new(),
            spent: false,
        });
        Ok(DeviceCode {
            device_code: SecretText::new(device_code),
            user_code: format!("FAKE-{n:04}"),
            verification_uri: DEVICE_VERIFICATION_URI.into(),
            expires_in_secs: s.expires_in_secs,
            interval_secs: s.interval_secs,
        })
    }

    fn issue(s: &mut State) -> UserTokens {
        s.next += 1;
        let n = s.next;
        let access = format!("ghu_fakeaccess{n:04}");
        s.issued.push(access.clone());
        let refresh = s.refresh_secs.map(|_| {
            let token = format!("ghr_fakerefresh{n:04}");
            s.issued.push(token.clone());
            s.live_refresh.insert(token.clone());
            SecretText::new(token)
        });
        UserTokens {
            access_token: SecretText::new(access),
            expires_in_secs: s.access_secs,
            refresh_token: refresh,
            refresh_expires_in_secs: s.refresh_secs,
        }
    }

    pub(crate) fn poll(&self, device_code: &SecretText) -> PortResult<DevicePoll> {
        let mut s = self.state();
        self.record(&mut s, "device_poll", device_code.expose());
        let Some(index) = s
            .devices
            .iter()
            .position(|d| d.device_code == device_code.expose())
        else {
            return Ok(DevicePoll::Failed {
                code: AuthorizationFailure::IncorrectDeviceCode,
            });
        };
        if s.devices[index].spent {
            return Ok(DevicePoll::Expired);
        }
        let answer = s.devices[index]
            .answers
            .pop_front()
            .unwrap_or(DeviceAnswer::Pending);
        Ok(match answer {
            DeviceAnswer::Pending => DevicePoll::Pending,
            DeviceAnswer::SlowDown(interval_secs) => DevicePoll::SlowDown { interval_secs },
            DeviceAnswer::Deny => DevicePoll::Denied,
            DeviceAnswer::Expire => DevicePoll::Expired,
            DeviceAnswer::Fail(code) => DevicePoll::Failed { code },
            DeviceAnswer::Approve => {
                s.devices[index].spent = true;
                DevicePoll::Token(FakeAccounts::issue(&mut s))
            }
        })
    }

    pub(crate) async fn refresh(&self, refresh_token: &SecretText) -> PortResult<TokenRefresh> {
        tokio::task::yield_now().await;
        let mut s = self.state();
        self.record(&mut s, "refresh", refresh_token.expose());
        if s.refresh_outages > 0 {
            s.refresh_outages -= 1;
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "status": 502 }),
            ));
        }
        if !s.live_refresh.remove(refresh_token.expose()) {
            return Ok(TokenRefresh::Rejected);
        }
        Ok(TokenRefresh::Refreshed(FakeAccounts::issue(&mut s)))
    }

    pub(crate) fn identity(&self, token: &SecretText) -> PortResult<IdentityLookup> {
        let mut s = self.state();
        self.record(&mut s, "user", token.expose());
        if s.identity_rejected {
            return Ok(IdentityLookup::Rejected { status: 401 });
        }
        let u = s.user.clone();
        Ok(IdentityLookup::Found(Identity {
            provider_user_id: u.id,
            email: format!("{}+{}@users.noreply.github.com", u.id, u.login),
            login: u.login,
            display_name: u.name,
        }))
    }
}
