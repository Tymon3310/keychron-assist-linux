# Keychron Assist cross-screen protocol (working notes)

Status legend: **[live]** = confirmed against the real Windows 1.1.6 client in the VM;
**[bin]** = read from the macOS binary (symbolized); **[guess]** = not confirmed.

## Ports [bin][live]

| Port | Proto | Use |
|---|---|---|
| 7654 | UDP | Discovery beacons (broadcast + SSDP multicast 239.255.255.250) |
| 40000 | UDP | `discover_port` default (GlobalServerPort) |
| 40001 | UDP | Session channel: pairing, heartbeat, input, file transfer |
| 40003 | TCP/WS | Local WebSocket server (`global_ws_channel`) |

Packets on 40001 are sent from an ephemeral source port; reply to `peer_ip:40001`, not to the source port.
Windows uses two different source sockets (one for control, one for input), so don't filter on source port.

## Discovery [live]

Once per second, both formats:

```
DISCOVER_JSON:{"device_id":..,"addr":"192.168.1.29","name":..,"os_info":"Linux 7.2","interface_name":"eno1",
               "network_type":"ethernet","mac_address":..,"session_id":..,"screen_resolution":{"width":..,"height":..},"remember":false}
DISCOVER:<name>:<os_info>
```

## Envelope [bin][live]

```json
{"evt_type":"RemoteDesk","evt_data":{"evt_type":"<RdpNotifyType>","evt_data":<T>},"source_addr":""}
```

`source_addr` is always a string ("" when unset). Compact serde_json, field order matters for byte-equality only.

RdpNotifyType: Discover, RequestPair, RequestConfirm, RequestReject, PairCancel, TrustUpdate, ScreenPosition,
RequestToC, FileName, FileStart, FileData, FileBatchEnd, FileBatchAck, FileEnd, FileMissing, FileCancel,
FileResult, FileTransferSwitch, LockScreen, Disconnect, Heartbeat, PeerDisconnected, LocalPairAccepted, LocalPairRejected.

## Pairing [live]

- `RequestPair` (initiator → peer, repeated a few times):
  `{"monitors":[{"width","height","is_primary","index"}],"os_info","screen_resolution":{..},"addr":"","name","device_id","session_id","dir":"Left|Right|..","automatic":false[,"trust_uuid"]}`
  `trust_uuid` is omitted when null. Receiver fills `addr` from the packet source.
- `RequestConfirm` (peer → initiator): `{"addr":"","device_id","name","os_info","session_id","screen_resolution"[,"trust_uuid"]}`
- `Disconnect` with `evt_data:""` ends a session.
- `Heartbeat` with `evt_data:""` every 1 s, both directions. Windows drops the link ~3.5–5 s after the last one
  (UI event `PeerDisconnected` `"network_timeout"`).
- Windows UI invokes on success: `switch_rdp_scan{flag:false}`, `rdp_complete_connection{addr,deviceId,deviceName,sessionId}`,
  `record_rdp_connection_history{device}`, `sync_rdp_web_state{state}`.

## Cross-screen control: `RequestToC` payload (RdpTocType) [bin][live]

Externally tagged serde enum, tuple variants:

| Variant | JSON | Notes |
|---|---|---|
| Init | `{"Init":[x_ratio, y_ratio]}` f32 | Sender's **exit** point as ratio of its own screen (Windows on our right exits at x=0.0). Receiver enters at its opposite edge at y_ratio. Receiver becomes slave. |
| Release | `{"Release":[x_ratio, y_ratio]}` f32 | Slave hands control back; ratio = slave's exit point. |
| MouseMove | `{"MouseMove":[dx, dy]}` i64 | Relative raw deltas, one packet per OS mouse event (~1–3 ms apart). |
| MouseClick | `{"MouseClick":["Left"\|"Right"\|..,"Press"\|"Release"]}` | `[MouseAction, InputState]` |
| MouseScroll | `{"MouseScroll":[dx, dy]}` i32 | Wheel notches; `[0,1]` = wheel up, `[0,-1]` = down. One packet per notch. |
| Keyboard | `{"Keyboard":["Press"\|"Release", code, plat]}` | `[InputState, i64, GlobalPlat]` |

### GlobalPlat [bin]

Serde names and indices: `Windows`(0), `MacOS`(1), `Linux`(2), `Android`(3), `IOS`(4), `Other`(5).
Unknown names (e.g. `"Win"`) make the whole packet fail to parse and it is dropped silently.

### Keyboard codes [bin][live]

- The Windows client always **sends** Windows virtual-key codes tagged `"Windows"`, with left/right-specific
  modifiers (VK_LCONTROL=162, VK_LSHIFT=160, ...). Captured: a=65, b=66, c=67, LCtrl=162, Enter=13.
- The Windows client **receives**: `MacOS` codes → translated from CGKeyCode to VK; every other tag → code used as VK.
- The macOS client **receives** only `Windows`-tagged codes (`handle_keyboard_action` returns early on plat≠0) and
  maps VK→CGKeyCode with `win_vk_to_cg_keycode`.
- So the Linux port must: **send** `[state, VK, "Windows"]`; **receive** `Windows`/others as VK, `MacOS` as CGKeyCode.

## Other session messages [live]

| Message | evt_data | When |
|---|---|---|
| RequestReject | `{"addr":""[,"trust_uuid"][,"reason"]}` (nulls omitted) | `rdp_reject` / user clicks Reject / automatic request fails trust check |
| PairCancel | `""` | initiator cancels a pending RequestPair (followed by `Disconnect ""`) |
| Disconnect | `""` | session end |
| TrustUpdate | `{"addr":"","trust_uuid":"<uuid>"}` | `rdp_trust_current_peer` |
| ScreenPosition | `"Left"\|"Right"\|..` | `rdp_update_screen_position{dir}`; sends the **mirrored** side (local "Right" → sends "Left", i.e. where the receiver's peer is) |
| FileTransferSwitch | `true`/`false` | `switch_file_transfer{v}` |
| LockScreen | `""` [guess, &str type exists] | lock shortcut |

Trust: when the receiver ticks "Trust this device", the UI generates a UUID and `RequestConfirm` carries `trust_uuid`.
Automatic reconnect (`automatic:true` + `trust_uuid` in RequestPair) is accepted only if the receiver's UI has that
device as its automatic-connection device and trust is mutual (logic lives in the Angular UI, `handleAutomaticPairRequest`);
otherwise it answers `RequestReject{trust_uuid}`.
The incoming-pair prompt is an in-app modal in the main window (30 s countdown, Reject/Agree, trust checkbox).

## File transfer [bin][live]

Payload tuples (from `RdpNotify<(..)>` generic types in the macOS binary, confirmed live):

| Message | Tuple | Notes |
|---|---|---|
| FileStart | `[id, name, chunk_count, size, id]` | sent 3×; 5th field equals id in all captures (history record id?) |
| FileData | `[id, index, base64]` | chunk = 1152 raw bytes (last one shorter), standard base64 with padding |
| FileBatchEnd | `[id, start, end]` | after every 32 chunks, `end` exclusive |
| FileBatchAck | `[id, start, end, [missing...]]` | receiver → sender; sender resends listed chunks and re-sends BatchEnd |
| FileEnd | `"id"` | after last batch acked; repeated until a result |
| FileMissing | `[id, [missing...]]` | receiver reply to FileEnd when chunks are missing |
| FileResult | `[id, ok, error\|null]` | receiver → sender, finishes the transfer |
| FileCancel | `"id"` | either side, sent 3×; UI shows `cancelled` / `peer_cancelled` |

Sender with no acks: retransmits FileStart + all BatchEnds with backoff ~0.36 s, 0.7, 1.4, 2.8, then every 3 s.
Windows streams a whole file's batches without waiting for each ack. Multiple files are queued one at a time.
Windows saves received files to `%USERPROFILE%\Downloads\<name>` (configurable via `set_rdp_file_folder`).
UI events: `FileTransfer {id,name,path,direction:"send"|"receive",status:"started"|"progress"|"completed"|"cancelled",
size_bytes,transferred_bytes,elapsed_ms,average_speed_bytes_per_second,instantaneous_speed_bytes_per_second,error}`.

## Behaviour details [live][bin]

- `RequestPair.dir` and `ScreenPosition` are both sent **mirrored**: the sender puts where *it* sits as seen from
  the receiver (Windows UI position Left → sends "Right"). The receiver uses the value as-is. "Off" exists too.
- `Init`/`Release` ratios are the sender's exit point on its own screen; the receiver enters at its edge facing the
  peer at the same ratio along that edge.
- Incoming `RequestPair`: backend emits UI `notify` RequestPair with `addr` = sender IP (bare, no port) and shows the
  `pair-confirm` window unless the request is `automatic` or carries a `trust_uuid` matching a trusted "passive"
  history record (the main window auto-accepts those). Retransmissions must be deduplicated.
- Pair-confirm window: `rdp_confirm_dialog{pair,trusted,trustUuid}` → send RequestConfirm, emit `LocalPairAccepted
  {pair,trusted,trust_uuid}`; `rdp_reject_dialog{pair}` → RequestReject, emit `LocalPairRejected`.
- Peer `FileTransferSwitch` is forwarded to the UI as `notify` FileTransferSwitch (it flips the toggle).
- Shortcuts are Shift + Esc + key (default disconnect key "A"); the lock shortcut sends `LockScreen ""` to the peer
  while controlling it, otherwise locks the local machine.
- File transfer history is recorded by the backend (the UI never calls `record_file_transfer_history`); statuses
  `sent` / `received` / `cancelled` / `failed` / `receive-error`, newest first, `error` omitted when null.
- Connection history is keyed by (device_id, connection_type "active"/"passive"), newest first.
- `get_plat` returns "Win" on Windows, "MacOs" on macOS (the UI only checks for "MacOs").
- Beacons: one `DISCOVER_JSON:` + one `DISCOVER:` per target per second; targets are multicast + subnet broadcast;
  virtual adapters (docker, VPN, tailscale, VMware, bridges...) are skipped.

## Still to capture

LockScreen payload, FileName (no sender seen), `assist.json` layout, Windows multi-monitor `monitors` list.

## Captures

`captures/win-master-input.jsonl` is a raw capture of Windows-as-master input (moves, keys, clicks, scroll).
