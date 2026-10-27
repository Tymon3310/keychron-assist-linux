use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Installed {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub icon_path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssistKeyBind {
    pub key: String,
    pub bind: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RdpSettings {
    #[serde(default)]
    pub automatic_connection_device_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReceiverVpidMap {
    pub receiver_vpid: String,
    pub device_vpid: String,
    pub device_type: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MosConf {
    pub speed: f64,
    pub step: f64,
    pub duration: f64,
    pub enable: bool,
    pub auto_enable: bool,
    pub detect_distance: f64,
    pub interval: u32,
    pub reverse_key: u32,
    pub speed_key: u32,
}

impl Default for MosConf {
    fn default() -> Self {
        Self {
            speed: 1.0,
            step: 10.0,
            duration: 0.5,
            enable: true,
            auto_enable: false,
            detect_distance: 5.0,
            interval: 16,
            reverse_key: 0,
            speed_key: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AiKeyConf {
    pub key: String,
    pub template: String,
    pub window: Value,
    pub dialog: Value,
    pub model: Value,
    pub vp_id: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QuickStartConfig {
    #[serde(default)]
    pub apps: Vec<Installed>,
    #[serde(default)]
    pub keybinds: HashMap<String, HashMap<String, Value>>,
    #[serde(default)]
    pub receiver_maps: HashMap<String, ReceiverVpidMap>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalJson {
    #[serde(default = "version")]
    pub version: String,
    #[serde(default)]
    pub quick_start: QuickStartConfig,
    #[serde(default)]
    pub mos: MosConf,
    #[serde(default)]
    pub ai_key: HashMap<String, Vec<Value>>,
}

fn version() -> String {
    "1.1.6".into()
}

impl Default for GlobalJson {
    fn default() -> Self {
        Self {
            version: version(),
            quick_start: QuickStartConfig::default(),
            mos: MosConf::default(),
            ai_key: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsRequest {
    pub evt_type: String,
    pub evt_data: WsData,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsData {
    pub cmd: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsResponse {
    pub evt_type: String,
    pub evt_data: WsData,
}

impl WsResponse {
    pub fn new(evt_type: &str, cmd: &str, data: Value) -> Self {
        Self {
            evt_type: evt_type.into(),
            evt_data: WsData {
                cmd: cmd.into(),
                data,
            },
        }
    }
}
