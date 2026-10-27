use crate::{
    linux::apps,
    model::{MosConf, ReceiverVpidMap, WsRequest, WsResponse},
    state::AppState,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{process::Command, sync::Arc};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const ADDR: &str = "127.0.0.1:40003";
const LEGACY_ADDR: &str = "127.0.0.1:23333";

pub async fn run(state: Arc<AppState>, outbound: broadcast::Sender<Value>) {
    let listener = match TcpListener::bind(ADDR).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[launcher-ws] bind {ADDR}: {e}");
            return;
        }
    };
    eprintln!("[launcher-ws] listening ws://{ADDR}");
    if let Ok(legacy) = TcpListener::bind(LEGACY_ADDR).await {
        eprintln!("[launcher-ws] legacy listener ws://{LEGACY_ADDR}");
        let legacy_state = state.clone();
        let legacy_out = outbound.clone();
        tokio::spawn(async move {
            accept_loop(legacy, legacy_state, legacy_out).await;
        });
    }
    accept_loop(listener, state, outbound).await;
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<AppState>,
    outbound: broadcast::Sender<Value>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let s = state.clone();
                let tx = outbound.clone();
                tokio::spawn(async move {
                    connection(stream, s, tx).await;
                });
            }
            Err(e) => eprintln!("[launcher-ws] accept: {e}"),
        }
    }
}

async fn connection(stream: TcpStream, state: Arc<AppState>, outbound: broadcast::Sender<Value>) {
    let Ok(socket) = accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = socket.split();
    let mut rx = outbound.subscribe();
    loop {
        tokio::select! {
            msg = source.next() => {
                let Some(Ok(Message::Text(text))) = msg else { break };
                let Ok(req) = serde_json::from_str::<WsRequest>(&text) else { continue };
                for response in dispatch(&state, req).await {
                    if sink.send(Message::Text(response.to_string().into())).await.is_err() { return; }
                }
            }
            msg = rx.recv() => {
                match msg {
                    Ok(value) => if sink.send(Message::Text(value.to_string().into())).await.is_err() { return },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}

async fn dispatch(state: &Arc<AppState>, req: WsRequest) -> Vec<Value> {
    let evt = req.evt_type.as_str();
    let cmd = req.evt_data.cmd.as_str();
    // The web launcher `JSON.stringify`s every `data` payload, so objects,
    // numbers, and ids arrive as JSON-encoded strings (e.g. `"{\"vp_id\":1}"`,
    // `"13364"`, `""`). Decode one layer before handling.
    let data = normalize_data(req.evt_data.data);
    let mut out = Vec::new();
    match (evt, cmd) {
        ("Common", "GetVersion") => {
            out.push(WsResponse::new("Common", cmd, json!({"version":"1.1.6"})).into())
        }
        ("Common", "OpenApp") => out.push(WsResponse::new("Common", cmd, json!(true)).into()),
        ("SysCmd", "SpawnAppByPath") => {
            let success = data.as_str().is_some_and(|p| open_target(p));
            out.push(WsResponse::new(evt, cmd, json!(success)).into());
        }
        ("QuickStart", "Get") => {
            let scanned = apps::scan();
            {
                let mut cfg = state.config.write().await;
                cfg.quick_start.apps = scanned.clone();
            }
            let _ = state.save_config().await;
            let n = scanned.len();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!({"action":"FetchStart","count":n,"data":null}),
                )
                .into(),
            );
            out.extend(scanned.into_iter().map(|item| {
                WsResponse::new(evt, cmd, json!({"action":"Fetch","count":n,"data":item})).into()
            }));
            out.push(
                WsResponse::new(evt, cmd, json!({"action":"FetchEnd","count":n,"data":null}))
                    .into(),
            );
        }
        ("QuickStart", "SelectAppByNative") => {
            let picked = rfd::FileDialog::new().pick_file();
            let items = picked
                .into_iter()
                .map(|path| {
                    let p = path.to_string_lossy().into_owned();
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Application")
                        .to_string();
                    json!([{"name":name,"path":p,"icon":"","icon_path":p}])
                })
                .next()
                .unwrap_or_else(|| json!([]));
            out.push(WsResponse::new("QuickStart", "AddApp", items).into());
        }
        ("QuickStart", "AddKeyBind") => {
            let vpid = string_key(data.get("vp_id"));
            let key = string_key(data.get("key"));
            let bind = data.get("bind").cloned().unwrap_or(Value::Null);
            // The Launcher treats a non-"Success" reply as failure and rolls
            // back, with a dedicated message for "KeyDuplicate".
            let mut cfg = state.config.write().await;
            let slot = cfg.quick_start.keybinds.entry(vpid).or_default();
            let code = if slot.get(&key).is_some_and(|existing| *existing != bind) {
                "KeyDuplicate"
            } else {
                slot.insert(key, bind);
                "Success"
            };
            drop(cfg);
            let code = if code == "Success" && state.save_config().await.is_err() {
                "CommandFailed"
            } else {
                code
            };
            out.push(WsResponse::new(evt, cmd, json!(code)).into());
        }
        ("QuickStart", "UpdateKeyBind") => {
            let vpid = string_key(data.get("vp_id"));
            let key = string_key(data.get("key"));
            let bind = data.get("bind").cloned().unwrap_or(Value::Null);
            let mut cfg = state.config.write().await;
            cfg.quick_start
                .keybinds
                .entry(vpid)
                .or_default()
                .insert(key, bind);
            drop(cfg);
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("QuickStart", "RemoveKeyBind") => {
            let vpid = string_key(data.get("vp_id"));
            let key = string_key(data.get("key"));
            let removed = state
                .config
                .write()
                .await
                .quick_start
                .keybinds
                .get_mut(&vpid)
                .is_some_and(|m| m.remove(&key).is_some());
            let ok = !removed || state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("QuickStart", "GetKeyBinds") => {
            let vpid = string_key(Some(&data));
            let cfg = state.config.read().await;
            let list = cfg
                .quick_start
                .keybinds
                .get(&vpid)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(key, bind)| json!({"key":key,"bind":bind}))
                .collect::<Vec<_>>();
            out.push(WsResponse::new(evt, cmd, json!(list)).into());
        }
        ("QuickStart", "ClearKeyBind") => {
            let vpid = string_key(Some(&data));
            let mut cfg = state.config.write().await;
            if vpid.is_empty() || vpid == "0" {
                cfg.quick_start.keybinds.clear();
            } else {
                cfg.quick_start.keybinds.remove(&vpid);
            }
            drop(cfg);
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("QuickStart", "SetReceiverVpidMap") => {
            if let Ok(map) = serde_json::from_value::<ReceiverVpidMap>(data) {
                state
                    .config
                    .write()
                    .await
                    .quick_start
                    .receiver_maps
                    .insert(map.receiver_vpid.clone(), map);
            }
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("QuickStart", "ClearReceiverVpidMap") => {
            let vpid = string_key(data.get("receiver_vpid"));
            state
                .config
                .write()
                .await
                .quick_start
                .receiver_maps
                .remove(&vpid);
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("AIKey", "Get") => {
            let vpid = string_key(Some(&data));
            let list = state
                .config
                .read()
                .await
                .ai_key
                .get(&vpid)
                .cloned()
                .unwrap_or_default();
            out.push(WsResponse::new(evt, cmd, json!(list)).into());
        }
        ("AIKey", "Set") => {
            let vpid = string_key(data.get("vp_id"));
            let mut cfg = state.config.write().await;
            let list = cfg.ai_key.entry(vpid).or_default();
            let duplicate = list.iter().any(|c| c.get("key") == data.get("key"));
            if !duplicate {
                list.push(data);
            }
            drop(cfg);
            let ok = !duplicate && state.save_config().await.is_ok();
            out.push(WsResponse::new(evt, cmd, json!(ok)).into());
        }
        ("AIKey", "Put") => {
            let vpid = string_key(data.pointer("/conf/vp_id"));
            let index = data
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX) as usize;
            let mut cfg = state.config.write().await;
            let updated = cfg
                .ai_key
                .get_mut(&vpid)
                .and_then(|list| list.get_mut(index))
                .zip(data.get("conf"))
                .map(|(slot, conf)| *slot = conf.clone())
                .is_some();
            drop(cfg);
            let ok = updated && state.save_config().await.is_ok();
            out.push(WsResponse::new(evt, cmd, json!(ok)).into());
        }
        ("AIKey", "Remove") => {
            let vpid = string_key(data.get("vp_id"));
            let index = data
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX) as usize;
            let removed = state
                .config
                .write()
                .await
                .ai_key
                .get_mut(&vpid)
                .is_some_and(|list| {
                    index < list.len() && {
                        list.remove(index);
                        true
                    }
                });
            let ok = !removed || state.save_config().await.is_ok();
            out.push(WsResponse::new(evt, cmd, json!(ok)).into());
        }
        ("AIKey", "Clear") => {
            let vpid = string_key(data.get("vp_id").or(Some(&data)));
            state.config.write().await.ai_key.remove(&vpid);
            let ok = state.save_config().await.is_ok();
            out.push(WsResponse::new(evt, cmd, json!(ok)).into());
        }
        ("Mos", "Get") => out.push(
            WsResponse::new(
                evt,
                cmd,
                serde_json::to_value(&state.config.read().await.mos).unwrap_or_default(),
            )
            .into(),
        ),
        ("Mos", "Set") => {
            if let Ok(conf) = serde_json::from_value::<MosConf>(data) {
                state.config.write().await.mos = conf;
            }
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        ("Mos", "Reset") => {
            state.config.write().await.mos = MosConf::default();
            let ok = state.save_config().await.is_ok();
            out.push(
                WsResponse::new(
                    evt,
                    cmd,
                    json!(if ok { "Success" } else { "CommandFailed" }),
                )
                .into(),
            );
        }
        _ => eprintln!("[launcher-ws] unhandled {evt}/{cmd}"),
    }
    out
}

fn string_key(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn normalize_data(data: Value) -> Value {
    match data {
        Value::String(s) if s.is_empty() => Value::Null,
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_data;
    use serde_json::{json, Value};
    #[test]
    fn launcher_stringified_payloads_decode_one_layer() {
        // Keybind payloads arrive as JSON-encoded strings.
        let decoded = normalize_data(Value::String(r#"{"vp_id":13364,"key":"0x7f00"}"#.into()));
        assert_eq!(decoded.get("vp_id").and_then(Value::as_u64), Some(13364));
        // Numeric ids arrive stringified too.
        assert_eq!(normalize_data(Value::String("13364".into())), json!(13364));
        // Empty-string payloads mean "no data".
        assert_eq!(normalize_data(Value::String(String::new())), Value::Null);
        // Already-structured values pass through untouched.
        let object = json!({"vp_id": "1:2"});
        assert_eq!(normalize_data(object.clone()), object);
    }

    fn test_state(root: &std::path::Path) -> std::sync::Arc<crate::state::AppState> {
        use tokio::sync::RwLock;
        std::fs::create_dir_all(root).unwrap();
        std::sync::Arc::new(crate::state::AppState {
            root: root.to_owned(),
            config: RwLock::new(Default::default()),
            language: RwLock::new("en-US".into()),
            translations: RwLock::new(json!({})),
        })
    }

    fn ws_request(evt_type: &str, cmd: &str, data: Value) -> crate::model::WsRequest {
        crate::model::WsRequest {
            evt_type: evt_type.into(),
            evt_data: crate::model::WsData {
                cmd: cmd.into(),
                data,
            },
        }
    }

    #[tokio::test]
    async fn keybind_crud_round_trips_launcher_payloads() {
        let root = std::env::temp_dir().join(format!(
            "assist-ws-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = test_state(&root);
        let bind = json!({"opt_type":"App","opt_data":{"name":"App","path":"/bin/true"}});

        // Add with a launcher-style stringified payload.
        let add = ws_request(
            "QuickStart",
            "AddKeyBind",
            Value::String(json!({"vp_id":13364,"key":"0x7f00","bind":bind}).to_string()),
        );
        let out = super::dispatch(&state, add).await;
        assert_eq!(out[0]["evt_data"]["data"], "Success");

        // Same content again stays a success (idempotent retry).
        let add_same = ws_request(
            "QuickStart",
            "AddKeyBind",
            Value::String(json!({"vp_id":13364,"key":"0x7f00","bind":bind}).to_string()),
        );
        let out = super::dispatch(&state, add_same).await;
        assert_eq!(out[0]["evt_data"]["data"], "Success");

        // Same key with a different bind is a conflict.
        let add_other = ws_request(
            "QuickStart",
            "AddKeyBind",
            Value::String(
                json!({"vp_id":13364,"key":"0x7f00","bind":{"opt_type":"Url"}}).to_string(),
            ),
        );
        let out = super::dispatch(&state, add_other).await;
        assert_eq!(out[0]["evt_data"]["data"], "KeyDuplicate");

        // GetKeyBinds with a stringified numeric vpid.
        let get = ws_request("QuickStart", "GetKeyBinds", Value::String("13364".into()));
        let out = super::dispatch(&state, get).await;
        let list = out[0]["evt_data"]["data"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["key"], "0x7f00");
        assert_eq!(list[0]["bind"], bind);

        // Remove then verify empty.
        let remove = ws_request(
            "QuickStart",
            "RemoveKeyBind",
            Value::String(json!({"vp_id":13364,"key":"0x7f00"}).to_string()),
        );
        let out = super::dispatch(&state, remove).await;
        assert_eq!(out[0]["evt_data"]["data"], "Success");
        let get = ws_request("QuickStart", "GetKeyBinds", Value::String("13364".into()));
        let out = super::dispatch(&state, get).await;
        assert!(out[0]["evt_data"]["data"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

fn open_target(target: &str) -> bool {
    if target.starts_with("https://")
        || target.starts_with("http://")
        || target.starts_with("file://")
    {
        return Command::new("xdg-open").arg(target).spawn().is_ok();
    }
    let Ok(args) = shell_words::split(target) else {
        return false;
    };
    let Some((program, rest)) = args.split_first() else {
        return false;
    };
    Command::new(program).args(rest).spawn().is_ok()
}

impl From<WsResponse> for Value {
    fn from(value: WsResponse) -> Self {
        serde_json::to_value(value).expect("serializable response")
    }
}
