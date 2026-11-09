//! Lectura del árbol de UI de Android (`uiautomator dump`) para tocar elementos por su texto o
//! id. Lo usa el cambio de cuenta con un clic.

/// Un nodo del dump con lo necesario para encontrarlo y tocarlo.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    pub text: String,
    pub desc: String,
    pub id: String,
    /// App a la que pertenece el nodo.
    pub package: String,
    /// Centro del rectángulo del nodo, en px de pantalla.
    pub center: (i32, i32),
    /// El nodo responde a toques (los textos dentro de un botón no).
    pub clickable: bool,
}

/// Script que imprime el XML del árbol de UI actual.
pub const DUMP_SCRIPT: &str = "uiautomator dump /data/local/tmp/pc_ui.xml >/dev/null 2>&1; \
     cat /data/local/tmp/pc_ui.xml; rm -f /data/local/tmp/pc_ui.xml";

/// Extrae los nodos del XML de `uiautomator dump`.
pub fn parse_nodes(xml: &str) -> Vec<UiNode> {
    xml.split("<node ")
        .skip(1)
        .filter_map(|chunk| {
            // Espacio delante: así el primer atributo también se encuentra como " nombre=".
            let tag = format!(" {}", &chunk[..chunk.find('>').unwrap_or(chunk.len())]);
            let tag = tag.as_str();
            let center = parse_bounds(&attr(tag, "bounds")?)?;
            Some(UiNode {
                text: attr(tag, "text").unwrap_or_default(),
                desc: attr(tag, "content-desc").unwrap_or_default(),
                id: attr(tag, "resource-id").unwrap_or_default(),
                package: attr(tag, "package").unwrap_or_default(),
                center,
                clickable: attr(tag, "clickable").is_some_and(|v| v == "true"),
            })
        })
        .collect()
}

/// Primer nodo cuyo texto es exactamente `name` (sin distinguir mayúsculas).
pub fn find_text<'a>(nodes: &'a [UiNode], name: &str) -> Option<&'a UiNode> {
    let name = name.trim();
    nodes.iter().find(|n| n.text.trim().eq_ignore_ascii_case(name))
}

pub fn find_id<'a>(nodes: &'a [UiNode], id: &str) -> Option<&'a UiNode> {
    nodes.iter().find(|n| n.id == id)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(unescape(&tag[start..end]))
}

/// `"[x1,y1][x2,y2]"` -> centro.
fn parse_bounds(b: &str) -> Option<(i32, i32)> {
    let nums: Vec<i32> = b
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums[..] {
        [x1, y1, x2, y2] => Some(((x1 + x2) / 2, (y1 + y2) / 2)),
        _ => None,
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="com.instagram.android:id/profile_tab" class="android.widget.FrameLayout" content-desc="Perfil" bounds="[576,1456][720,1544]"><node index="0" text="Mi_Cuenta.1" resource-id="" class="android.view.View" content-desc="" bounds="[168,550][271,591]" /></node><node text="a &amp; b" bounds="[0,0][10,10]" /></hierarchy>"#;

    #[test]
    fn parses_nodes_and_centers() {
        let nodes = parse_nodes(XML);
        assert_eq!(nodes.len(), 3);
        let tab = find_id(&nodes, "com.instagram.android:id/profile_tab").unwrap();
        assert_eq!(tab.center, (648, 1500));
        assert_eq!(tab.desc, "Perfil");
        let acc = find_text(&nodes, "mi_cuenta.1").unwrap();
        assert_eq!(acc.center, (219, 570));
        assert_eq!(nodes[2].text, "a & b");
    }
}
