use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::InspectorError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiBounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl UiBounds {
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiNode {
    pub id: String,
    pub class_name: String,
    pub package_name: String,
    pub resource_id: String,
    pub text: String,
    pub content_desc: String,
    pub clickable: bool,
    pub enabled: bool,
    pub focused: bool,
    pub scrollable: bool,
    pub selected: bool,
    pub bounds: UiBounds,
    pub children: Vec<UiNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub serial: String,
    pub screen_width: u32,
    pub screen_height: u32,
    pub screenshot_data_url: String,
    pub roots: Vec<UiNode>,
    pub node_count: usize,
}

/// Captures a device PNG screenshot and `uiautomator` XML hierarchy dump in parallel.
pub async fn capture_layout_snapshot(
    serial: Option<&str>,
) -> Result<LayoutSnapshot, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let (png_res, xml_res) = tokio::join!(
        capture_screenshot_png(&dev.serial),
        dump_uiautomator_xml(&dev.serial)
    );

    let png_bytes = png_res?;
    let xml = xml_res?;

    let (mut width, mut height) = parse_png_dimensions(&png_bytes).unwrap_or((1080, 1920));
    let (roots, node_count) = parse_uiautomator_xml(&xml)?;

    // Fallback dimensions from root bounds if PNG header was unavailable
    if (width == 0 || height == 0) && !roots.is_empty() {
        width = roots[0].bounds.right.max(1) as u32;
        height = roots[0].bounds.bottom.max(1) as u32;
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok(LayoutSnapshot {
        serial: dev.serial,
        screen_width: width,
        screen_height: height,
        screenshot_data_url: format!("data:image/png;base64,{b64}"),
        roots,
        node_count,
    })
}

async fn capture_screenshot_png(serial: &str) -> Result<Vec<u8>, InspectorError> {
    let out = Command::new("adb")
        .args(["-s", serial, "exec-out", "screencap", "-p"])
        .output()
        .await?;
    if !out.status.success() || out.stdout.len() < 24 {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(InspectorError::Message(format!(
            "screencap failed: {}",
            if stderr.is_empty() {
                "empty PNG output"
            } else {
                &stderr
            }
        )));
    }
    Ok(out.stdout)
}

async fn dump_uiautomator_xml(serial: &str) -> Result<String, InspectorError> {
    // Fast path: dump directly to stdout (`/dev/tty`)
    let out = Command::new("adb")
        .args(["-s", serial, "exec-out", "uiautomator", "dump", "/dev/tty"])
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if let Some(xml) = extract_hierarchy_xml(&stdout) {
        return Ok(xml);
    }

    // Fallback: dump to temp file on device and read back
    let dump = Command::new("adb")
        .args([
            "-s",
            serial,
            "shell",
            "uiautomator dump /data/local/tmp/android_lens_ui.xml >/dev/null && cat /data/local/tmp/android_lens_ui.xml && rm -f /data/local/tmp/android_lens_ui.xml",
        ])
        .output()
        .await?;
    let fallback_out = String::from_utf8_lossy(&dump.stdout).to_string();
    if let Some(xml) = extract_hierarchy_xml(&fallback_out) {
        return Ok(xml);
    }

    let stderr = String::from_utf8_lossy(&dump.stderr).trim().to_string();
    Err(InspectorError::Message(format!(
        "uiautomator dump failed: {}",
        if stderr.is_empty() {
            "no <hierarchy> XML returned (is the screen awake?)"
        } else {
            &stderr
        }
    )))
}

fn extract_hierarchy_xml(raw: &str) -> Option<String> {
    let start = raw.find("<hierarchy")?;
    let end = raw.rfind("</hierarchy>")?;
    Some(raw[start..end + "</hierarchy>".len()].to_string())
}

/// Reads PNG width & height directly from the standard 24-byte PNG IHDR header.
pub fn parse_png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

/// Parses `[x1,y1][x2,y2]` bounds attribute from `uiautomator`.
pub fn parse_bounds(raw: &str) -> UiBounds {
    let nums: Vec<i32> = raw
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<i32>().ok())
        .collect();
    if nums.len() >= 4 {
        UiBounds {
            left: nums[0],
            top: nums[1],
            right: nums[2],
            bottom: nums[3],
        }
    } else {
        UiBounds {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        }
    }
}

/// Zero-dependency parser for Android `uiautomator` `<hierarchy><node .../></hierarchy>` XML.
pub fn parse_uiautomator_xml(xml: &str) -> Result<(Vec<UiNode>, usize), InspectorError> {
    let bytes = xml.as_bytes();
    let mut pos = 0usize;
    let mut stack: Vec<UiNode> = Vec::new();
    let mut roots: Vec<UiNode> = Vec::new();
    let mut counter: usize = 0;

    while pos < bytes.len() {
        let Some(lt_rel) = xml[pos..].find('<') else {
            break;
        };
        pos += lt_rel + 1;
        if pos >= bytes.len() {
            break;
        }

        // Skip XML declaration / comments / processing instructions
        if xml[pos..].starts_with("!--") {
            if let Some(end) = xml[pos..].find("-->") {
                pos += end + 3;
                continue;
            }
            break;
        }
        if xml[pos..].starts_with('?') || xml[pos..].starts_with('!') {
            if let Some(end) = xml[pos..].find('>') {
                pos += end + 1;
                continue;
            }
            break;
        }

        // Find end of tag respecting quoted attribute values
        let Some(tag_end) = find_tag_end(xml, pos) else {
            break;
        };
        let tag_content = xml[pos..tag_end].trim();
        pos = tag_end + 1;

        if let Some(closing) = tag_content.strip_prefix('/') {
            let name = closing.trim();
            if name == "node" {
                if let Some(finished) = stack.pop() {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(finished);
                    } else {
                        roots.push(finished);
                    }
                }
            }
            continue;
        }

        let (inner, self_closing) = match tag_content.strip_suffix('/') {
            Some(s) => (s.trim_end(), true),
            None => (tag_content, false),
        };

        let mut split = inner.splitn(2, |c: char| c.is_whitespace());
        let tag_name = split.next().unwrap_or("");
        let attr_str = split.next().unwrap_or("");

        if tag_name != "node" {
            continue;
        }

        counter += 1;
        let attrs = parse_xml_attrs(attr_str);
        let get = |k: &str| {
            attrs
                .iter()
                .find(|(ak, _)| ak == k)
                .map(|(_, av)| av.as_str())
                .unwrap_or("")
        };

        let node = UiNode {
            id: format!("n-{counter}"),
            class_name: get("class").to_string(),
            package_name: get("package").to_string(),
            resource_id: get("resource-id").to_string(),
            text: get("text").to_string(),
            content_desc: get("content-desc").to_string(),
            clickable: get("clickable") == "true",
            enabled: get("enabled") != "false",
            focused: get("focused") == "true",
            scrollable: get("scrollable") == "true",
            selected: get("selected") == "true",
            bounds: parse_bounds(get("bounds")),
            children: Vec::new(),
        };

        if self_closing {
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            } else {
                roots.push(node);
            }
        } else {
            stack.push(node);
        }
    }

    // Flush any unclosed nodes gracefully
    while let Some(finished) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(finished);
        } else {
            roots.push(finished);
        }
    }

    Ok((roots, counter))
}

pub(crate) fn find_tag_end(s: &str, start: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let bytes = s.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

pub(crate) fn parse_xml_attrs(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = s[key_start..i].to_string();
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'=') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        let val = unescape_xml(&s[val_start..i]);
        if i < bytes.len() {
            i += 1;
        }
        if !key.is_empty() {
            attrs.push((key, val));
        }
    }
    attrs
}

pub(crate) fn unescape_xml(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let entity = &after[..semi];
            match entity {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    if let Some(ch) = u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        out.push(ch);
                    }
                }
                _ if entity.starts_with('#') => {
                    if let Some(ch) = entity[1..].parse::<u32>().ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                _ => {
                    out.push('&');
                    out.push_str(entity);
                    out.push(';');
                }
            }
            rest = &after[semi + 1..];
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uiautomator_hierarchy_and_bounds() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation="0">
  <node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.everybit.sholattv.native.dev" content-desc="" clickable="false" enabled="true" focused="false" scrollable="false" selected="false" bounds="[0,0][1920,1080]">
    <node index="0" text="Masjid Al-Hakim &amp; Co" resource-id="com.everybit.sholattv:id/title" class="android.widget.TextView" package="com.everybit.sholattv.native.dev" content-desc="Header &gt; Title" clickable="true" enabled="true" focused="true" scrollable="false" selected="false" bounds="[64,48][640,128]" />
  </node>
</hierarchy>"#;

        let (roots, count) = parse_uiautomator_xml(xml).unwrap();
        assert_eq!(count, 2);
        assert_eq!(roots.len(), 1);
        let root = &roots[0];
        assert_eq!(root.bounds.width(), 1920);
        assert_eq!(root.bounds.height(), 1080);
        assert_eq!(root.children.len(), 1);

        let child = &root.children[0];
        assert_eq!(child.text, "Masjid Al-Hakim & Co");
        assert_eq!(child.content_desc, "Header > Title");
        assert_eq!(child.resource_id, "com.everybit.sholattv:id/title");
        assert!(child.clickable);
        assert!(child.focused);
        assert_eq!(child.bounds.left, 64);
        assert_eq!(child.bounds.bottom, 128);
    }

    #[test]
    fn parses_png_ihdr_dimensions() {
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        hdr.extend_from_slice(&13u32.to_be_bytes());
        hdr.extend_from_slice(b"IHDR");
        hdr.extend_from_slice(&1920u32.to_be_bytes());
        hdr.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&hdr), Some((1920, 1080)));
    }
}
