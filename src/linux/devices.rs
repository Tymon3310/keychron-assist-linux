use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const QK_CR_FIRST: u16 = 0x7f00;
pub const QK_CR_LAST: u16 = 0x7f33;
pub const LINUX_KEY_MACRO1: u16 = 656;

pub fn vpid(vendor: u16, product: u16) -> String {
    format!("{vendor}:{product}")
}

pub fn enumerate_keychron() -> Vec<Value> {
    let mut list = BTreeMap::<(u16, u16), Value>::new();
    for (_, device) in evdev::enumerate() {
        let id = device.input_id();
        let name = device.name().unwrap_or_default();
        if id.vendor() != 0x3434
            && !name.to_ascii_lowercase().contains("keychron")
            && !name.to_ascii_lowercase().contains("lemokey")
        {
            continue;
        }
        let key_slots = device
            .supported_keys()
            .map(|keys| {
                keys.iter()
                    .filter(|k| {
                        (LINUX_KEY_MACRO1..=LINUX_KEY_MACRO1 + (QK_CR_LAST - QK_CR_FIRST))
                            .contains(&k.0)
                    })
                    .count()
            })
            .unwrap_or(0);
        let entry = list.entry((id.vendor(), id.product())).or_insert_with(|| {
            json!({
                "vid":id.vendor(), "pid":id.product(), "vp_id":vpid(id.vendor(),id.product()),
                "device_name":name, "support_50_key":false, "support_mouse":false, "keyid":""
            })
        });
        if key_slots != 0 {
            entry["support_50_key"] = Value::Bool(true);
        }
        if device.supported_relative_axes().is_some() {
            entry["support_mouse"] = Value::Bool(true);
        }
    }
    list.into_values().collect()
}

pub fn qk_cr_slot(evdev_code: u16) -> Option<String> {
    let index = evdev_code.checked_sub(LINUX_KEY_MACRO1)?;
    if index > QK_CR_LAST - QK_CR_FIRST {
        return None;
    }
    Some(format!("0x{:04x}", QK_CR_FIRST + index))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumer_keycodes_map_to_qk_cr_slots() {
        assert_eq!(qk_cr_slot(656).as_deref(), Some("0x7f00"));
        assert_eq!(qk_cr_slot(707).as_deref(), Some("0x7f33"));
        assert_eq!(qk_cr_slot(708), None);
        assert_eq!(qk_cr_slot(256), None); // never conflate PB/mouse buttons
    }
}
