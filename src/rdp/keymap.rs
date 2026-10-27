//! Key code translation: Linux evdev <-> Windows virtual-key (wire format) <-> macOS CGKeyCode.
//!
//! The wire always carries Windows VK codes tagged `Windows` (that's what the Windows client
//! sends and the only thing the macOS client accepts). The Windows client also accepts
//! `MacOS`-tagged CGKeyCodes and raw VKs under any other tag, so incoming events are mapped by tag.

use super::wire::GlobalPlat;

/// (evdev code, Windows VK, macOS CGKeyCode)
const TABLE: &[(u16, u16, Option<u16>)] = &[
    (1, 0x1B, Some(53)),    // Esc
    (2, 0x31, Some(18)),    // 1
    (3, 0x32, Some(19)),    // 2
    (4, 0x33, Some(20)),    // 3
    (5, 0x34, Some(21)),    // 4
    (6, 0x35, Some(23)),    // 5
    (7, 0x36, Some(22)),    // 6
    (8, 0x37, Some(26)),    // 7
    (9, 0x38, Some(28)),    // 8
    (10, 0x39, Some(25)),   // 9
    (11, 0x30, Some(29)),   // 0
    (12, 0xBD, Some(27)),   // -
    (13, 0xBB, Some(24)),   // =
    (14, 0x08, Some(51)),   // Backspace
    (15, 0x09, Some(48)),   // Tab
    (16, 0x51, Some(12)),   // Q
    (17, 0x57, Some(13)),   // W
    (18, 0x45, Some(14)),   // E
    (19, 0x52, Some(15)),   // R
    (20, 0x54, Some(17)),   // T
    (21, 0x59, Some(16)),   // Y
    (22, 0x55, Some(32)),   // U
    (23, 0x49, Some(34)),   // I
    (24, 0x4F, Some(31)),   // O
    (25, 0x50, Some(35)),   // P
    (26, 0xDB, Some(33)),   // [
    (27, 0xDD, Some(30)),   // ]
    (28, 0x0D, Some(36)),   // Enter
    (29, 0xA2, Some(59)),   // Left Ctrl
    (30, 0x41, Some(0)),    // A
    (31, 0x53, Some(1)),    // S
    (32, 0x44, Some(2)),    // D
    (33, 0x46, Some(3)),    // F
    (34, 0x47, Some(5)),    // G
    (35, 0x48, Some(4)),    // H
    (36, 0x4A, Some(38)),   // J
    (37, 0x4B, Some(40)),   // K
    (38, 0x4C, Some(37)),   // L
    (39, 0xBA, Some(41)),   // ;
    (40, 0xDE, Some(39)),   // '
    (41, 0xC0, Some(50)),   // `
    (42, 0xA0, Some(56)),   // Left Shift
    (43, 0xDC, Some(42)),   // backslash
    (44, 0x5A, Some(6)),    // Z
    (45, 0x58, Some(7)),    // X
    (46, 0x43, Some(8)),    // C
    (47, 0x56, Some(9)),    // V
    (48, 0x42, Some(11)),   // B
    (49, 0x4E, Some(45)),   // N
    (50, 0x4D, Some(46)),   // M
    (51, 0xBC, Some(43)),   // ,
    (52, 0xBE, Some(47)),   // .
    (53, 0xBF, Some(44)),   // /
    (54, 0xA1, Some(60)),   // Right Shift
    (55, 0x6A, Some(67)),   // KP *
    (56, 0xA4, Some(58)),   // Left Alt / Option
    (57, 0x20, Some(49)),   // Space
    (58, 0x14, Some(57)),   // Caps Lock
    (59, 0x70, Some(122)),  // F1
    (60, 0x71, Some(120)),  // F2
    (61, 0x72, Some(99)),   // F3
    (62, 0x73, Some(118)),  // F4
    (63, 0x74, Some(96)),   // F5
    (64, 0x75, Some(97)),   // F6
    (65, 0x76, Some(98)),   // F7
    (66, 0x77, Some(100)),  // F8
    (67, 0x78, Some(101)),  // F9
    (68, 0x79, Some(109)),  // F10
    (69, 0x90, Some(71)),   // Num Lock / Clear
    (70, 0x91, None),       // Scroll Lock
    (71, 0x67, Some(89)),   // KP 7
    (72, 0x68, Some(91)),   // KP 8
    (73, 0x69, Some(92)),   // KP 9
    (74, 0x6D, Some(78)),   // KP -
    (75, 0x64, Some(86)),   // KP 4
    (76, 0x65, Some(87)),   // KP 5
    (77, 0x66, Some(88)),   // KP 6
    (78, 0x6B, Some(69)),   // KP +
    (79, 0x61, Some(83)),   // KP 1
    (80, 0x62, Some(84)),   // KP 2
    (81, 0x63, Some(85)),   // KP 3
    (82, 0x60, Some(82)),   // KP 0
    (83, 0x6E, Some(65)),   // KP .
    (86, 0xE2, Some(10)),   // ISO extra key
    (87, 0x7A, Some(103)),  // F11
    (88, 0x7B, Some(111)),  // F12
    (89, 0xC1, Some(94)),   // JIS Ro / underscore
    (92, 0x1C, None),       // Henkan / Convert
    (93, 0x15, Some(104)),  // Katakana/Hiragana / Kana
    (94, 0x1D, Some(102)),  // Muhenkan / Eisu
    (96, 0x0D, Some(76)),   // KP Enter (VK_RETURN on Windows)
    (97, 0xA3, Some(62)),   // Right Ctrl
    (98, 0x6F, Some(75)),   // KP /
    (99, 0x2C, Some(105)),  // Print Screen / SysRq (F13 on mac)
    (100, 0xA5, Some(61)),  // Right Alt / Option
    (102, 0x24, Some(115)), // Home
    (103, 0x26, Some(126)), // Up
    (104, 0x21, Some(116)), // Page Up
    (105, 0x25, Some(123)), // Left
    (106, 0x27, Some(124)), // Right
    (107, 0x23, Some(119)), // End
    (108, 0x28, Some(125)), // Down
    (109, 0x22, Some(121)), // Page Down
    (110, 0x2D, Some(114)), // Insert / Help
    (111, 0x2E, Some(117)), // Delete (forward)
    (113, 0xAD, Some(74)),  // Mute
    (114, 0xAE, Some(73)),  // Volume Down
    (115, 0xAF, Some(72)),  // Volume Up
    (117, 0x92, Some(81)),  // KP =
    (119, 0x13, None),      // Pause
    (121, 0x6C, Some(95)),  // KP , (JIS)
    (122, 0x15, None),      // Hangeul
    (123, 0x19, None),      // Hanja
    (124, 0xDC, Some(93)),  // JIS Yen (shares VK with backslash)
    (125, 0x5B, Some(55)),  // Left Meta / Win / Command
    (126, 0x5C, Some(54)),  // Right Meta
    (127, 0x5D, Some(110)), // Menu / Apps
    (128, 0xB2, None),      // Stop
    (140, 0xB7, None),      // Calculator
    (150, 0xAC, None),      // WWW / Browser Home
    (155, 0xB4, None),      // Mail
    (156, 0xAB, None),      // Bookmarks / Favorites
    (158, 0xA6, None),      // Back
    (159, 0xA7, None),      // Forward
    (163, 0xB0, None),      // Next track
    (164, 0xB3, None),      // Play/Pause
    (165, 0xB1, None),      // Previous track
    (166, 0xB2, None),      // Stop CD
    (172, 0xAC, None),      // Homepage
    (173, 0xA8, None),      // Refresh
    (183, 0x7C, Some(105)), // F13
    (184, 0x7D, Some(107)), // F14
    (185, 0x7E, Some(113)), // F15
    (186, 0x7F, Some(106)), // F16
    (187, 0x80, Some(64)),  // F17
    (188, 0x81, Some(79)),  // F18
    (189, 0x82, Some(80)),  // F19
    (190, 0x83, Some(90)),  // F20
    (191, 0x84, None),      // F21
    (192, 0x85, None),      // F22
    (193, 0x86, None),      // F23
    (194, 0x87, None),      // F24
    (217, 0xAA, None),      // Search
    (464, 0xFF, Some(63)),  // Fn (KEY_FN); VK 0xFF is unassigned on Windows
];

/// Linux key -> Windows VK for sending.
pub fn evdev_to_vk(code: u16) -> Option<u16> {
    TABLE.iter().find(|r| r.0 == code).map(|r| r.1).filter(|vk| *vk != 0xFF)
}

/// Incoming key code -> Linux key, according to the sender's platform tag.
pub fn wire_to_evdev(code: i64, plat: GlobalPlat) -> Option<u16> {
    let code = u16::try_from(code).ok()?;
    match plat {
        GlobalPlat::MacOS => TABLE.iter().find(|r| r.2 == Some(code)).map(|r| r.0),
        _ => vk_to_evdev(code),
    }
}

pub fn vk_to_evdev(vk: u16) -> Option<u16> {
    match vk {
        // Side-agnostic modifiers map to the left key.
        0x10 => Some(42),
        0x11 => Some(29),
        0x12 => Some(56),
        _ => TABLE.iter().find(|r| r.1 == vk).map(|r| r.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_windows_codes_map() {
        // Captured from the Windows client: a b c, LCtrl+C, Enter.
        assert_eq!(wire_to_evdev(65, GlobalPlat::Windows), Some(30));
        assert_eq!(wire_to_evdev(66, GlobalPlat::Windows), Some(48));
        assert_eq!(wire_to_evdev(67, GlobalPlat::Windows), Some(46));
        assert_eq!(wire_to_evdev(162, GlobalPlat::Windows), Some(29));
        assert_eq!(wire_to_evdev(13, GlobalPlat::Windows), Some(28));
        assert_eq!(wire_to_evdev(0, GlobalPlat::MacOS), Some(30));
        assert_eq!(wire_to_evdev(56, GlobalPlat::MacOS), Some(42));
    }

    #[test]
    fn round_trip_unique_vks() {
        for &(ev, vk, _) in TABLE {
            if vk == 0xFF {
                continue;
            }
            let back = vk_to_evdev(vk).unwrap();
            // Shared VKs resolve to the first row; both rows still send the same VK.
            assert_eq!(evdev_to_vk(back), Some(vk), "evdev {ev}");
        }
        assert_eq!(evdev_to_vk(30), Some(0x41));
        assert_eq!(evdev_to_vk(464), None);
    }
}
