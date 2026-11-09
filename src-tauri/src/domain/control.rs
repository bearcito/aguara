//! `ControlMessage` (PC -> teléfono) y su serialización exacta (§4.1).
//!
//! Opcodes: 0..=11 son scrcpy upstream; 96..=99 son extensiones del fork de Panda.

use super::binary::{float_to_i16_fixed_point, float_to_u16_fixed_point};

/// Coordenada + tamaño del frame de referencia donde se tomó (12 bytes en el wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
}

impl Position {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.x.to_be_bytes());
        out.extend_from_slice(&self.y.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
    }
}

// Opcodes del socket de control.
const TYPE_INJECT_KEYCODE: u8 = 0;
const TYPE_INJECT_TEXT: u8 = 1;
const TYPE_INJECT_TOUCH_EVENT: u8 = 2;
const TYPE_INJECT_SCROLL_EVENT: u8 = 3;
const TYPE_BACK_OR_SCREEN_ON: u8 = 4;
const TYPE_EXPAND_NOTIFICATION_PANEL: u8 = 5;
const TYPE_EXPAND_SETTINGS_PANEL: u8 = 6;
const TYPE_COLLAPSE_PANELS: u8 = 7;
const TYPE_GET_CLIPBOARD: u8 = 8;
const TYPE_SET_CLIPBOARD: u8 = 9;
const TYPE_SET_SCREEN_POWER_MODE: u8 = 10;
const TYPE_ROTATE_DEVICE: u8 = 11;
// Extensiones del fork.
const TYPE_SET_DISCONNECT: u8 = 96;
const TYPE_WALLPAPER: u8 = 97;
const TYPE_SET_PAUSE: u8 = 98;
const TYPE_SET_OPTIONS: u8 = 99;

/// Un mensaje de control serializable al formato del socket.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    InjectKeycode { action: u8, keycode: i32, repeat: i32, meta_state: i32 },
    InjectText { text: String },
    InjectTouch {
        action: u8,
        pointer_id: i64,
        position: Position,
        pressure: f32,
        action_button: i32,
        buttons: i32,
    },
    InjectScroll { position: Position, h_scroll: f32, v_scroll: f32, buttons: i32 },
    BackOrScreenOn { action: u8 },
    ExpandNotificationPanel,
    ExpandSettingsPanel,
    CollapsePanels,
    GetClipboard { copy_key: u8 },
    SetClipboard { sequence: i64, paste: bool, text: String },
    SetScreenPowerMode { mode: i8 },
    RotateDevice,
    // Extensiones del fork:
    SetDisconnect,
    Wallpaper { value: String },
    SetPause { value: i8 },
    SetOptions { value: String },
}

impl ControlMessage {
    /// Serializa al formato binario del socket de control (big-endian).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        match self {
            ControlMessage::InjectKeycode { action, keycode, repeat, meta_state } => {
                out.push(TYPE_INJECT_KEYCODE);
                out.push(*action);
                out.extend_from_slice(&keycode.to_be_bytes());
                out.extend_from_slice(&repeat.to_be_bytes());
                out.extend_from_slice(&meta_state.to_be_bytes());
            }
            ControlMessage::InjectText { text } => {
                out.push(TYPE_INJECT_TEXT);
                write_string(&mut out, text);
            }
            ControlMessage::InjectTouch {
                action, pointer_id, position, pressure, action_button, buttons,
            } => {
                out.push(TYPE_INJECT_TOUCH_EVENT);
                out.push(*action);
                out.extend_from_slice(&pointer_id.to_be_bytes());
                position.write(&mut out);
                out.extend_from_slice(&float_to_u16_fixed_point(*pressure).to_be_bytes());
                out.extend_from_slice(&action_button.to_be_bytes());
                out.extend_from_slice(&buttons.to_be_bytes());
            }
            ControlMessage::InjectScroll { position, h_scroll, v_scroll, buttons } => {
                out.push(TYPE_INJECT_SCROLL_EVENT);
                position.write(&mut out);
                out.extend_from_slice(&float_to_i16_fixed_point(*h_scroll).to_be_bytes());
                out.extend_from_slice(&float_to_i16_fixed_point(*v_scroll).to_be_bytes());
                out.extend_from_slice(&buttons.to_be_bytes());
            }
            ControlMessage::BackOrScreenOn { action } => {
                out.push(TYPE_BACK_OR_SCREEN_ON);
                out.push(*action);
            }
            ControlMessage::ExpandNotificationPanel => out.push(TYPE_EXPAND_NOTIFICATION_PANEL),
            ControlMessage::ExpandSettingsPanel => out.push(TYPE_EXPAND_SETTINGS_PANEL),
            ControlMessage::CollapsePanels => out.push(TYPE_COLLAPSE_PANELS),
            ControlMessage::GetClipboard { copy_key } => {
                out.push(TYPE_GET_CLIPBOARD);
                out.push(*copy_key);
            }
            ControlMessage::SetClipboard { sequence, paste, text } => {
                out.push(TYPE_SET_CLIPBOARD);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.push(if *paste { 1 } else { 0 });
                write_string(&mut out, text);
            }
            ControlMessage::SetScreenPowerMode { mode } => {
                out.push(TYPE_SET_SCREEN_POWER_MODE);
                out.push(*mode as u8);
            }
            ControlMessage::RotateDevice => out.push(TYPE_ROTATE_DEVICE),
            ControlMessage::SetDisconnect => out.push(TYPE_SET_DISCONNECT),
            ControlMessage::Wallpaper { value } => {
                out.push(TYPE_WALLPAPER);
                write_string(&mut out, value);
            }
            ControlMessage::SetPause { value } => {
                out.push(TYPE_SET_PAUSE);
                out.push(*value as u8);
            }
            ControlMessage::SetOptions { value } => {
                out.push(TYPE_SET_OPTIONS);
                write_string(&mut out, value);
            }
        }
        out
    }
}

/// String en el wire: `len` (i32 BE) seguido de los bytes UTF-8.
fn write_string(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycode_layout() {
        // type(1) + action(1) + keycode(4) + repeat(4) + meta(4) = 14 bytes
        let m = ControlMessage::InjectKeycode { action: 0, keycode: 66, repeat: 0, meta_state: 0 };
        let b = m.encode();
        assert_eq!(b.len(), 14);
        assert_eq!(b[0], 0);
        assert_eq!(b[1], 0);
        assert_eq!(&b[2..6], &66i32.to_be_bytes());
    }

    #[test]
    fn touch_layout_is_32_bytes() {
        // type(1) + 31 de payload (ver §4.1)
        let m = ControlMessage::InjectTouch {
            action: 0,
            pointer_id: -1,
            position: Position { x: 100, y: 200, width: 1080, height: 1920 },
            pressure: 1.0,
            action_button: 1,
            buttons: 1,
        };
        let b = m.encode();
        assert_eq!(b.len(), 32);
        assert_eq!(b[0], 2);
        // pressure 1.0 -> 0xFFFF en los 2 bytes tras la posición (offset 1+1+8+12 = 22)
        assert_eq!(&b[22..24], &[0xFF, 0xFF]);
    }

    #[test]
    fn scroll_layout_is_21_bytes() {
        let m = ControlMessage::InjectScroll {
            position: Position { x: 0, y: 0, width: 1080, height: 1920 },
            h_scroll: 0.0,
            v_scroll: 1.0,
            buttons: 0,
        };
        let b = m.encode();
        assert_eq!(b.len(), 21);
        assert_eq!(b[0], 3);
        // v_scroll 1.0 -> 0x7FFF (offset 1 + 12 + 2 = 15)
        assert_eq!(&b[15..17], &[0x7F, 0xFF]);
    }

    #[test]
    fn text_is_length_prefixed_utf8() {
        let m = ControlMessage::InjectText { text: "hi".into() };
        let b = m.encode();
        assert_eq!(b, vec![1, 0, 0, 0, 2, b'h', b'i']);
    }

    #[test]
    fn empty_messages_are_single_byte() {
        assert_eq!(ControlMessage::CollapsePanels.encode(), vec![7]);
        assert_eq!(ControlMessage::RotateDevice.encode(), vec![11]);
        assert_eq!(ControlMessage::SetDisconnect.encode(), vec![96]);
    }

    #[test]
    fn fork_options_opcode() {
        let b = ControlMessage::SetOptions { value: "x".into() }.encode();
        assert_eq!(b, vec![99, 0, 0, 0, 1, b'x']);
    }
}
