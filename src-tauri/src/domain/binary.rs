//! Conversión de punto fijo del protocolo scrcpy (`Binary` en el fork).

/// `pressure` f32 en [0.0, 1.0] -> u16. 1.0 se mapea a 0xFFFF (igual que el fork).
pub fn float_to_u16_fixed_point(v: f32) -> u16 {
    let v = v.clamp(0.0, 1.0);
    let scaled = (v * 65536.0) as u32;
    if scaled >= 0xFFFF { 0xFFFF } else { scaled as u16 }
}

/// `scroll` f32 en [-1.0, 1.0] -> i16. 1.0 se mapea a 0x7FFF (igual que el fork).
pub fn float_to_i16_fixed_point(v: f32) -> i16 {
    let v = v.clamp(-1.0, 1.0);
    let scaled = (v * 32768.0) as i32;
    if scaled >= 0x7FFF {
        0x7FFF
    } else if scaled < -0x8000 {
        -0x8000
    } else {
        scaled as i16
    }
}

/// Inversa de `float_to_u16_fixed_point` (para tests / DeviceMessage).
pub fn u16_fixed_point_to_float(s: u16) -> f32 {
    if s == 0xFFFF { 1.0 } else { s as f32 / 65536.0 }
}
