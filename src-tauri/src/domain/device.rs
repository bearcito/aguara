//! Ficha de un dispositivo: estado, cuentas registradas y apps instaladas.
//!
//! Todo sale de UNA invocación de `adb shell` con [`DETAILS_SCRIPT`] (secciones marcadas con
//! `==NOMBRE==`), que [`parse_details`] convierte en [`DeviceDetails`]. Puro: sin I/O.

use serde::Serialize;

/// Script de shell que relevan los datos de la ficha, una sección por marcador.
pub const DETAILS_SCRIPT: &str = "echo ==PROPS==; getprop ro.product.brand; \
getprop ro.product.model; getprop ro.build.version.release; \
echo ==BATT==; dumpsys battery; \
echo ==DF==; df /data; \
echo ==MEM==; grep -E 'MemTotal|MemAvailable' /proc/meminfo; \
echo ==WAKE==; dumpsys power 2>/dev/null | grep -m1 mWakefulness=; \
echo ==IP==; ip -f inet addr show wlan0 2>/dev/null; \
echo ==ACC==; dumpsys account 2>/dev/null | grep 'Account {name='; \
echo ==PKG==; pm list packages 2>/dev/null; \
echo ==END==";

/// Cuenta registrada en el AccountManager de Android (p. ej. `com.google`, `www.instagram.com`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Account {
    /// Tipo de cuenta (identifica la app/red).
    pub kind: String,
    /// Nombre visible (usuario, email…).
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DeviceDetails {
    pub brand: String,
    pub model: String,
    pub android: String,
    pub battery_level: Option<u8>,
    pub charging: bool,
    pub storage_total_kb: Option<u64>,
    pub storage_free_kb: Option<u64>,
    pub ram_total_kb: Option<u64>,
    pub ram_available_kb: Option<u64>,
    pub screen_on: Option<bool>,
    pub ip: Option<String>,
    pub accounts: Vec<Account>,
    /// Paquetes instalados (la UI filtra los que le interesan).
    pub packages: Vec<String>,
}

/// Interpreta la salida de [`DETAILS_SCRIPT`]. Campos faltantes quedan en `None`/vacío.
pub fn parse_details(out: &str) -> DeviceDetails {
    let mut d = DeviceDetails::default();
    let mut section = "";
    let mut props = Vec::new();
    for raw in out.lines() {
        let line = raw.trim();
        if let Some(name) = line.strip_prefix("==").and_then(|l| l.strip_suffix("==")) {
            section = name;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        match section {
            "PROPS" => props.push(line.to_string()),
            "BATT" => {
                if let Some(v) = line.strip_prefix("level:") {
                    d.battery_level = v.trim().parse().ok();
                } else if let Some(v) = line.strip_prefix("status:") {
                    // BatteryManager: 2 = cargando, 5 = completa (enchufado).
                    d.charging = matches!(v.trim(), "2" | "5");
                }
            }
            "DF" => {
                // Filesystem 1K-blocks Used Available Use% Mounted
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 && cols[1].bytes().all(|b| b.is_ascii_digit()) {
                    d.storage_total_kb = cols[1].parse().ok();
                    d.storage_free_kb = cols[3].parse().ok();
                }
            }
            "MEM" => {
                let kb = || line.split_whitespace().nth(1).and_then(|v| v.parse().ok());
                if line.starts_with("MemTotal:") {
                    d.ram_total_kb = kb();
                } else if line.starts_with("MemAvailable:") {
                    d.ram_available_kb = kb();
                }
            }
            "WAKE" => {
                if let Some(v) = line.split("mWakefulness=").nth(1) {
                    d.screen_on = Some(v.trim() == "Awake");
                }
            }
            "IP" => {
                if let Some(rest) = line.strip_prefix("inet ") {
                    let ip = rest.split(['/', ' ']).next().unwrap_or_default();
                    if !ip.is_empty() && d.ip.is_none() {
                        d.ip = Some(ip.to_string());
                    }
                }
            }
            "ACC" => {
                if let Some(acc) = parse_account(line) {
                    if !d.accounts.contains(&acc) {
                        d.accounts.push(acc);
                    }
                }
            }
            "PKG" => {
                if let Some(p) = line.strip_prefix("package:") {
                    d.packages.push(p.to_string());
                }
            }
            _ => {}
        }
    }
    let mut props = props.into_iter();
    d.brand = props.next().unwrap_or_default();
    d.model = props.next().unwrap_or_default();
    d.android = props.next().unwrap_or_default();
    d
}

/// `Account {name=juan@x.com, type=com.google}` -> Account. El nombre puede tener comas, por
/// eso se corta en el ÚLTIMO `, type=`.
fn parse_account(line: &str) -> Option<Account> {
    let body = line.strip_prefix("Account {name=")?.strip_suffix('}')?;
    let (name, kind) = body.rsplit_once(", type=")?;
    Some(Account { kind: kind.to_string(), name: name.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "==PROPS==
Redmi
25028RN03A
15
==BATT==
Current Battery Service state:
  AC powered: false
  USB powered: true
  status: 2
  level: 47
==DF==
Filesystem       1K-blocks     Used Available Use% Mounted on
/dev/block/dm-57 113122304 18619676  94371556  17% /data/user/0
==MEM==
MemTotal:        3885556 kB
MemAvailable:    2453340 kB
==WAKE==
  mWakefulness=Awake
==IP==
32: wlan0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500
    inet 192.168.1.23/24 brd 192.168.1.255 scope global wlan0
==ACC==
    Account {name=ana@gmail.com, type=com.google}
    Account {name=Ana, Pérez, type=com.facebook.auth.login}
    Account {name=ana.cm, type=www.instagram.com}
    Account {name=ana.cm, type=www.instagram.com}
==PKG==
package:com.instagram.android
package:com.whatsapp
==END==
";

    #[test]
    fn parses_full_sample() {
        let d = parse_details(SAMPLE);
        assert_eq!((d.brand.as_str(), d.model.as_str(), d.android.as_str()), ("Redmi", "25028RN03A", "15"));
        assert_eq!(d.battery_level, Some(47));
        assert!(d.charging);
        assert_eq!(d.storage_total_kb, Some(113122304));
        assert_eq!(d.storage_free_kb, Some(94371556));
        assert_eq!(d.ram_total_kb, Some(3885556));
        assert_eq!(d.ram_available_kb, Some(2453340));
        assert_eq!(d.screen_on, Some(true));
        assert_eq!(d.ip.as_deref(), Some("192.168.1.23"));
        // Duplicados eliminados; nombre con coma preservado.
        assert_eq!(d.accounts.len(), 3);
        assert_eq!(d.accounts[1], Account { kind: "com.facebook.auth.login".into(), name: "Ana, Pérez".into() });
        assert_eq!(d.packages, vec!["com.instagram.android", "com.whatsapp"]);
    }

    #[test]
    fn tolerates_missing_sections() {
        let d = parse_details("==PROPS==\nSamsung\n==WAKE==\n  mWakefulness=Asleep\n");
        assert_eq!(d.brand, "Samsung");
        assert_eq!(d.model, "");
        assert_eq!(d.screen_on, Some(false));
        assert!(d.accounts.is_empty());
        assert_eq!(d.battery_level, None);
    }
}
