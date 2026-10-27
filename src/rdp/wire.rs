//! Wire format of the Keychron Assist cross-screen protocol (UDP, serde_json).
//! See `re/PROTOCOL.md` for how each shape was confirmed.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DISCOVERY_PORT: u16 = 7654;
pub const SESSION_PORT: u16 = 40001;
pub const MULTICAST_GROUP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(239, 255, 255, 250);

/// Raw bytes per `FileData` chunk and chunks per acknowledged batch.
pub const FILE_CHUNK: usize = 1152;
pub const FILE_BATCH: u64 = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RdpDir {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}

impl RdpDir {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ScreenResolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub index: usize,
}

/// Discovery beacon body (`DISCOVER_JSON:` prefix) and the UI's `Discover` event payload.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerClient {
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub os_info: String,
    #[serde(default)]
    pub interface_name: Option<String>,
    #[serde(default)]
    pub network_type: Option<String>,
    #[serde(default)]
    pub mac_address: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub screen_resolution: Option<ScreenResolution>,
    #[serde(default)]
    pub remember: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestPairInfo {
    #[serde(default)]
    pub monitors: Vec<Monitor>,
    #[serde(default)]
    pub os_info: String,
    #[serde(default)]
    pub screen_resolution: Option<ScreenResolution>,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub dir: RdpDir,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_uuid: Option<String>,
    #[serde(default)]
    pub automatic: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmInfo {
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub os_info: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub screen_resolution: Option<ScreenResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_uuid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RejectInfo {
    #[serde(default)]
    pub addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustInfo {
    #[serde(default)]
    pub addr: String,
    pub trust_uuid: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputState {
    Press,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Platform tag on `Keyboard` events. Windows and macOS clients both send
/// Windows virtual-key codes tagged `Windows`; macOS only accepts that tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlobalPlat {
    Windows,
    MacOS,
    Linux,
    Android,
    IOS,
    Other,
}

/// `RequestToC` payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Toc {
    /// Sender's exit point as a ratio of its own screen; receiver becomes controlled.
    Init(f32, f32),
    /// Controlled side hands control back; ratio of its exit point.
    Release(f32, f32),
    MouseMove(i64, i64),
    MouseClick(MouseAction, InputState),
    /// Wheel notches: `[horizontal, vertical]`, positive vertical = up.
    MouseScroll(i32, i32),
    Keyboard(InputState, i64, GlobalPlat),
}

/// Outer envelope: `{"evt_type":"RemoteDesk","evt_data":{"evt_type":kind,"evt_data":data},"source_addr":""}`.
pub fn packet(kind: &str, data: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "evt_type": "RemoteDesk",
        "evt_data": {"evt_type": kind, "evt_data": data},
        "source_addr": ""
    }))
    .expect("packet serializes")
}

/// Parse a session datagram into `(kind, data)`.
pub fn parse(bytes: &[u8]) -> Option<(String, Value)> {
    let mut v: Value = serde_json::from_slice(bytes).ok()?;
    if v.get("evt_type")?.as_str()? != "RemoteDesk" {
        return None;
    }
    let inner = v.get_mut("evt_data")?.take();
    let kind = inner.get("evt_type")?.as_str()?.to_owned();
    let data = inner.get("evt_data").cloned().unwrap_or(Value::Null);
    Some((kind, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_shapes_match_captures() {
        let s = |t: &Toc| serde_json::to_string(t).unwrap();
        assert_eq!(s(&Toc::Init(1.0, 0.5)), r#"{"Init":[1.0,0.5]}"#);
        assert_eq!(s(&Toc::MouseMove(-1, 65)), r#"{"MouseMove":[-1,65]}"#);
        assert_eq!(
            s(&Toc::MouseClick(MouseAction::Left, InputState::Press)),
            r#"{"MouseClick":["Left","Press"]}"#
        );
        assert_eq!(s(&Toc::MouseScroll(0, -1)), r#"{"MouseScroll":[0,-1]}"#);
        assert_eq!(
            s(&Toc::Keyboard(InputState::Press, 65, GlobalPlat::Windows)),
            r#"{"Keyboard":["Press",65,"Windows"]}"#
        );
        let r: Toc = serde_json::from_str(r#"{"Release":[0.0,0.5389408]}"#).unwrap();
        assert!(matches!(r, Toc::Release(x, _) if x == 0.0));
    }

    #[test]
    fn envelope_round_trips() {
        let p = packet("Heartbeat", json!(""));
        assert_eq!(
            std::str::from_utf8(&p).unwrap(),
            // byte-identical to the Windows client's packets
            r#"{"evt_type":"RemoteDesk","evt_data":{"evt_type":"Heartbeat","evt_data":""},"source_addr":""}"#
        );
        let (k, d) = parse(&p).unwrap();
        assert_eq!(k, "Heartbeat");
        assert_eq!(d, json!(""));
    }

    #[test]
    fn pair_info_omits_null_trust() {
        let p: RequestPairInfo = serde_json::from_str(r#"{"monitors":[{"width":2522,"height":1284,"is_primary":true,"index":0}],"os_info":"Windows 11 (26200)","screen_resolution":{"width":2522,"height":1284},"addr":"","name":"DESKTOP","device_id":"cb60","session_id":"3780-1","dir":"Left","automatic":false}"#).unwrap();
        assert_eq!(p.dir, RdpDir::Left);
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("trust_uuid").is_none());
        let r = serde_json::to_value(RejectInfo { addr: String::new(), trust_uuid: None, reason: None }).unwrap();
        assert_eq!(r, json!({"addr":""}));
    }
}
