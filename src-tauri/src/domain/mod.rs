//! Capa de dominio: modelo y códec del protocolo de control.
//!
//! Formato fijado por ingeniería inversa del fork scrcpy de Panda
//! (ver `docs/PANDA_PROTOCOL.md` §4.1). Todos los enteros son **big-endian**.
//! Esta capa es pura: sin I/O, sin dependencias de Tauri ni de sockets.

mod binary;
pub mod control;
pub mod device;
pub mod ui;

pub use control::{ControlMessage, Position};
pub use device::{parse_details, DeviceDetails, DETAILS_SCRIPT};
