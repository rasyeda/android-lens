use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

use crate::uiautomator::{find_tag_end, parse_xml_attrs, unescape_xml};
use crate::InspectorError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStorageIndex {
    pub serial: String,
    pub package: String,
    pub shared_prefs: Vec<String>,
    pub datastores: Vec<String>,
    pub databases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefEntry {
    pub key: String,
    pub value_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteTableData {
    pub database: String,
    pub table: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
}

/// Validates a relative storage filename returned by `ls` to avoid path traversal or shell injection.
fn validate_file_name(name: &str) -> Result<&str, InspectorError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(InspectorError::Message(format!(
            "invalid storage filename '{name}'"
        )));
    }
    Ok(trimmed)
}

/// Discovers SharedPreferences (`.xml`), Jetpack DataStore (`files/datastore/*`),
/// and SQLite databases (`databases/*`) inside a debuggable app via `adb shell run-as <pkg>`.
pub async fn list_app_storage(
    serial: Option<&str>,
    package: &str,
) -> Result<AppStorageIndex, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;

    let (prefs, ds, dbs) = tokio::join!(
        run_as_ls(&dev.serial, package, "shared_prefs"),
        run_as_ls(&dev.serial, package, "files/datastore"),
        run_as_ls(&dev.serial, package, "databases"),
    );

    let shared_prefs = prefs.into_iter().filter(|f| f.ends_with(".xml")).collect();

    let datastores = ds
        .into_iter()
        .filter(|f| f.ends_with(".preferences_pb") || f.ends_with(".pb"))
        .collect();

    let databases = dbs
        .into_iter()
        .filter(|f| !f.ends_with("-journal") && !f.ends_with("-wal") && !f.ends_with("-shm"))
        .collect();

    Ok(AppStorageIndex {
        serial: dev.serial,
        package: package.to_string(),
        shared_prefs,
        datastores,
        databases,
    })
}

async fn run_as_ls(serial: &str, package: &str, subdir: &str) -> Vec<String> {
    let out = Command::new("adb")
        .args(["-s", serial, "shell", "run-as", package, "ls", "-1", subdir])
        .output()
        .await;
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut items: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("No such file") && !l.starts_with("ls:"))
        .map(str::to_string)
        .collect();
    items.sort();
    items
}

async fn run_as_cat_bytes(
    serial: &str,
    package: &str,
    rel_path: &str,
) -> Result<Vec<u8>, InspectorError> {
    let out = Command::new("adb")
        .args(["-s", serial, "exec-out", "run-as", package, "cat", rel_path])
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(InspectorError::Message(format!(
            "run-as cat '{rel_path}' failed: {}",
            if stderr.is_empty() {
                "app must be debuggable"
            } else {
                &stderr
            }
        )));
    }
    Ok(out.stdout)
}

/// Reads and parses an Android `shared_prefs/<file>.xml` file.
pub async fn read_shared_prefs(
    serial: Option<&str>,
    package: &str,
    file_name: &str,
) -> Result<Vec<PrefEntry>, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let name = validate_file_name(file_name)?;
    let bytes = run_as_cat_bytes(&dev.serial, package, &format!("shared_prefs/{name}")).await?;
    let xml = String::from_utf8_lossy(&bytes);
    Ok(parse_shared_prefs_xml(&xml))
}

/// Reads and decodes a Jetpack Preferences DataStore (`files/datastore/<file>.preferences_pb`) file.
pub async fn read_datastore_prefs(
    serial: Option<&str>,
    package: &str,
    file_name: &str,
) -> Result<Vec<PrefEntry>, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let name = validate_file_name(file_name)?;
    let bytes = run_as_cat_bytes(&dev.serial, package, &format!("files/datastore/{name}")).await?;
    parse_preferences_pb(&bytes)
}

/// Pulls a SQLite database (plus optional `-wal` and `-shm` sidecars) to a temporary host folder
/// and lists all user tables.
pub async fn list_sqlite_tables(
    serial: Option<&str>,
    package: &str,
    db_name: &str,
) -> Result<Vec<String>, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let name = validate_file_name(db_name)?;
    let tmp = pull_sqlite_bundle(&dev.serial, package, name).await?;
    let res = sqlite_list_tables_local(&tmp.db_path).await;
    drop(tmp);
    res
}

/// Executes a read-only SQLite query (`SELECT` or table browse) against a pulled copy of `databases/<db_name>`.
pub async fn query_sqlite_table(
    serial: Option<&str>,
    package: &str,
    db_name: &str,
    table_or_sql: &str,
    is_custom_sql: bool,
    limit: usize,
) -> Result<SqliteTableData, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let name = validate_file_name(db_name)?;
    let tmp = pull_sqlite_bundle(&dev.serial, package, name).await?;
    let res = sqlite_query_local(&tmp.db_path, name, table_or_sql, is_custom_sql, limit).await;
    drop(tmp);
    res
}

struct TempSqliteGuard {
    dir: PathBuf,
    db_path: PathBuf,
}

impl Drop for TempSqliteGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn pull_sqlite_bundle(
    serial: &str,
    package: &str,
    db_name: &str,
) -> Result<TempSqliteGuard, InspectorError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("android-lens-db-{stamp}"));
    std::fs::create_dir_all(&dir)?;

    let main_bytes = run_as_cat_bytes(serial, package, &format!("databases/{db_name}")).await?;
    let db_path = dir.join(db_name);
    std::fs::write(&db_path, &main_bytes)?;

    // Also pull WAL/SHM sidecars if present so Room/WAL-mode databases reflect latest writes.
    for suffix in ["-wal", "-shm"] {
        let sidecar = format!("{db_name}{suffix}");
        if let Ok(bytes) = run_as_cat_bytes(serial, package, &format!("databases/{sidecar}")).await
        {
            if !bytes.is_empty() {
                let _ = std::fs::write(dir.join(&sidecar), &bytes);
            }
        }
    }

    Ok(TempSqliteGuard { dir, db_path })
}

async fn run_sqlite3(db_path: &Path, sql: &str, json_mode: bool) -> Result<String, InspectorError> {
    let mut cmd = Command::new("sqlite3");
    cmd.arg("-readonly");
    if json_mode {
        cmd.arg("-json");
    }
    cmd.arg(db_path).arg(sql);

    let out = cmd.output().await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            InspectorError::Message(
                "sqlite3 CLI not found on PATH (required for SQLite table browsing)".into(),
            )
        } else {
            InspectorError::Io(e)
        }
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(InspectorError::Message(format!("sqlite3 error: {stderr}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn sqlite_list_tables_local(db_path: &Path) -> Result<Vec<String>, InspectorError> {
    let out = run_sqlite3(
        db_path,
        "SELECT name FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name;",
        false,
    )
    .await?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub async fn sqlite_query_local(
    db_path: &Path,
    db_name: &str,
    table_or_sql: &str,
    is_custom_sql: bool,
    limit: usize,
) -> Result<SqliteTableData, InspectorError> {
    let lim = limit.clamp(1, 500);
    let (sql, label) = if is_custom_sql {
        let trimmed = table_or_sql.trim();
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("select")
            || lower.starts_with("pragma")
            || lower.starts_with("with"))
        {
            return Err(InspectorError::Message(
                "only read-only SELECT / WITH / PRAGMA queries are allowed".into(),
            ));
        }
        (trimmed.to_string(), "query".to_string())
    } else {
        let escaped = table_or_sql.replace('"', "\"\"");
        (
            format!("SELECT * FROM \"{escaped}\" LIMIT {lim};"),
            table_or_sql.to_string(),
        )
    };

    let raw_json = run_sqlite3(db_path, &sql, true).await?;
    let trimmed = raw_json.trim();
    if trimmed.is_empty() {
        // Fetch column names via PRAGMA table_info when browsing an empty table
        let mut cols = Vec::new();
        if !is_custom_sql {
            let escaped = table_or_sql.replace('\'', "''");
            if let Ok(info) =
                run_sqlite3(db_path, &format!("PRAGMA table_info('{escaped}');"), true).await
            {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(info.trim()) {
                    cols = items
                        .into_iter()
                        .filter_map(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect();
                }
            }
        }
        return Ok(SqliteTableData {
            database: db_name.to_string(),
            table: label,
            columns: cols,
            rows: Vec::new(),
            total_rows: 0,
        });
    }

    let parsed: Value = serde_json::from_str(trimmed)
        .map_err(|e| InspectorError::Message(format!("failed to parse sqlite3 JSON: {e}")))?;
    let arr = parsed.as_array().cloned().unwrap_or_default();

    let mut columns: Vec<String> = Vec::new();
    if let Some(Value::Object(first)) = arr.first() {
        columns = first.keys().cloned().collect();
    }

    let mut rows = Vec::with_capacity(arr.len());
    for item in &arr {
        if let Value::Object(map) = item {
            let row = columns
                .iter()
                .map(|c| match map.get(c) {
                    None | Some(Value::Null) => "NULL".to_string(),
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                })
                .collect();
            rows.push(row);
        }
    }

    let total_rows = rows.len();
    Ok(SqliteTableData {
        database: db_name.to_string(),
        table: label,
        columns,
        rows,
        total_rows,
    })
}

/// Parses Android `SharedPreferences` XML (`<map><string>...</string><boolean .../>...</map>`).
pub fn parse_shared_prefs_xml(xml: &str) -> Vec<PrefEntry> {
    let mut entries = Vec::new();
    let bytes = xml.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() {
        let Some(lt) = xml[pos..].find('<') else {
            break;
        };
        pos += lt + 1;
        if pos >= bytes.len() {
            break;
        }
        if xml[pos..].starts_with('?') || xml[pos..].starts_with('!') {
            if let Some(end) = xml[pos..].find('>') {
                pos += end + 1;
                continue;
            }
            break;
        }

        let Some(tag_end) = find_tag_end(xml, pos) else {
            break;
        };
        let raw_tag = xml[pos..tag_end].trim();
        pos = tag_end + 1;

        if raw_tag.starts_with('/') {
            continue;
        }
        let (inner, self_closing) = match raw_tag.strip_suffix('/') {
            Some(s) => (s.trim_end(), true),
            None => (raw_tag, false),
        };
        let mut split = inner.splitn(2, |c: char| c.is_whitespace());
        let tag_name = split.next().unwrap_or("");
        let attrs = parse_xml_attrs(split.next().unwrap_or(""));
        let get = |k: &str| {
            attrs
                .iter()
                .find(|(ak, _)| ak == k)
                .map(|(_, av)| av.as_str())
                .unwrap_or("")
        };

        match tag_name {
            "int" | "long" | "float" | "boolean" => {
                let key = get("name").to_string();
                let value = get("value").to_string();
                if !key.is_empty() {
                    entries.push(PrefEntry {
                        key,
                        value_type: tag_name.to_string(),
                        value,
                    });
                }
            }
            "string" => {
                let key = get("name").to_string();
                let value = if self_closing {
                    String::new()
                } else if let Some(close_rel) = xml[pos..].find("</string>") {
                    let body = unescape_xml(&xml[pos..pos + close_rel]);
                    pos += close_rel + "</string>".len();
                    body
                } else {
                    String::new()
                };
                if !key.is_empty() {
                    entries.push(PrefEntry {
                        key,
                        value_type: "string".to_string(),
                        value,
                    });
                }
            }
            "set" => {
                let key = get("name").to_string();
                let mut items = Vec::new();
                if !self_closing {
                    if let Some(close_rel) = xml[pos..].find("</set>") {
                        let block = &xml[pos..pos + close_rel];
                        let mut bpos = 0;
                        while let Some(s_open) = block[bpos..].find("<string>") {
                            let start = bpos + s_open + "<string>".len();
                            if let Some(s_close) = block[start..].find("</string>") {
                                items.push(unescape_xml(&block[start..start + s_close]));
                                bpos = start + s_close + "</string>".len();
                            } else {
                                break;
                            }
                        }
                        pos += close_rel + "</set>".len();
                    }
                }
                if !key.is_empty() {
                    entries.push(PrefEntry {
                        key,
                        value_type: "string_set".to_string(),
                        value: serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()),
                    });
                }
            }
            _ => {}
        }
    }

    entries.sort_by_key(|e| e.key.to_lowercase());
    entries
}

/// Pure-Rust protobuf decoder for Jetpack DataStore `PreferencesProto.PreferenceMap` (`*.preferences_pb`).
///
/// Schema:
/// `message PreferenceMap { map<string, Value> preferences = 1; }`
/// `message Value { oneof value { bool boolean=1; float float=2; int32 integer=3; int64 long=4; string string=5; StringSet string_set=6; double double=7; bytes bytes=8; } }`
pub fn parse_preferences_pb(data: &[u8]) -> Result<Vec<PrefEntry>, InspectorError> {
    let mut entries = Vec::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let (field_num, wire_type) = read_pb_key(data, &mut pos)?;
        if field_num == 1 && wire_type == 2 {
            let entry_bytes = read_pb_bytes(data, &mut pos)?;
            if let Some(entry) = decode_pb_map_entry(entry_bytes)? {
                entries.push(entry);
            }
        } else {
            skip_pb_field(data, &mut pos, wire_type)?;
        }
    }

    entries.sort_by_key(|e| e.key.to_lowercase());
    Ok(entries)
}

fn decode_pb_map_entry(buf: &[u8]) -> Result<Option<PrefEntry>, InspectorError> {
    let mut pos = 0usize;
    let mut key = String::new();
    let mut val_type = String::from("unknown");
    let mut val_str = String::new();

    while pos < buf.len() {
        let (field_num, wire_type) = read_pb_key(buf, &mut pos)?;
        match (field_num, wire_type) {
            (1, 2) => {
                let s = read_pb_bytes(buf, &mut pos)?;
                key = String::from_utf8_lossy(s).to_string();
            }
            (2, 2) => {
                let vbuf = read_pb_bytes(buf, &mut pos)?;
                let (t, v) = decode_pb_value(vbuf)?;
                val_type = t;
                val_str = v;
            }
            _ => skip_pb_field(buf, &mut pos, wire_type)?,
        }
    }

    if key.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PrefEntry {
            key,
            value_type: val_type,
            value: val_str,
        }))
    }
}

fn decode_pb_value(buf: &[u8]) -> Result<(String, String), InspectorError> {
    let mut pos = 0usize;
    while pos < buf.len() {
        let (field_num, wire_type) = read_pb_key(buf, &mut pos)?;
        match (field_num, wire_type) {
            (1, 0) => {
                let v = read_pb_varint(buf, &mut pos)?;
                return Ok(("boolean".into(), (v != 0).to_string()));
            }
            (2, 5) => {
                let b = read_pb_fixed(buf, &mut pos, 4)?;
                let v = f32::from_le_bytes(b.try_into().unwrap());
                return Ok(("float".into(), v.to_string()));
            }
            (3, 0) => {
                let v = read_pb_varint(buf, &mut pos)? as i32;
                return Ok(("int".into(), v.to_string()));
            }
            (4, 0) => {
                let v = read_pb_varint(buf, &mut pos)? as i64;
                return Ok(("long".into(), v.to_string()));
            }
            (5, 2) => {
                let s = read_pb_bytes(buf, &mut pos)?;
                return Ok(("string".into(), String::from_utf8_lossy(s).to_string()));
            }
            (6, 2) => {
                let set_buf = read_pb_bytes(buf, &mut pos)?;
                let mut spos = 0usize;
                let mut items = Vec::new();
                while spos < set_buf.len() {
                    let (sf, sw) = read_pb_key(set_buf, &mut spos)?;
                    if sf == 1 && sw == 2 {
                        let item = read_pb_bytes(set_buf, &mut spos)?;
                        items.push(String::from_utf8_lossy(item).to_string());
                    } else {
                        skip_pb_field(set_buf, &mut spos, sw)?;
                    }
                }
                return Ok((
                    "string_set".into(),
                    serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()),
                ));
            }
            (7, 1) => {
                let b = read_pb_fixed(buf, &mut pos, 8)?;
                let v = f64::from_le_bytes(b.try_into().unwrap());
                return Ok(("double".into(), v.to_string()));
            }
            (8, 2) => {
                let raw = read_pb_bytes(buf, &mut pos)?;
                return Ok(("bytes".into(), format!("<{} bytes>", raw.len())));
            }
            _ => skip_pb_field(buf, &mut pos, wire_type)?,
        }
    }
    Ok(("empty".into(), String::new()))
}

fn read_pb_varint(buf: &[u8], pos: &mut usize) -> Result<u64, InspectorError> {
    let mut val = 0u64;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() || shift >= 64 {
            return Err(InspectorError::Message("truncated protobuf varint".into()));
        }
        let b = buf[*pos];
        *pos += 1;
        val |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok(val);
        }
        shift += 7;
    }
}

fn read_pb_key(buf: &[u8], pos: &mut usize) -> Result<(u32, u8), InspectorError> {
    let tag = read_pb_varint(buf, pos)?;
    Ok(((tag >> 3) as u32, (tag & 0x7) as u8))
}

fn read_pb_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8], InspectorError> {
    let len = read_pb_varint(buf, pos)? as usize;
    if *pos + len > buf.len() {
        return Err(InspectorError::Message(
            "truncated protobuf length-delimited field".into(),
        ));
    }
    let slice = &buf[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

fn read_pb_fixed<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], InspectorError> {
    if *pos + n > buf.len() {
        return Err(InspectorError::Message(
            "truncated protobuf fixed field".into(),
        ));
    }
    let slice = &buf[*pos..*pos + n];
    *pos += n;
    Ok(slice)
}

fn skip_pb_field(buf: &[u8], pos: &mut usize, wire_type: u8) -> Result<(), InspectorError> {
    match wire_type {
        0 => {
            read_pb_varint(buf, pos)?;
            Ok(())
        }
        1 => {
            read_pb_fixed(buf, pos, 8)?;
            Ok(())
        }
        2 => {
            read_pb_bytes(buf, pos)?;
            Ok(())
        }
        5 => {
            read_pb_fixed(buf, pos, 4)?;
            Ok(())
        }
        other => Err(InspectorError::Message(format!(
            "unsupported protobuf wire type {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shared_preferences_xml_all_types() {
        let xml = r#"<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="masjid_name">Masjid Al-Hakim &amp; Center</string>
    <boolean name="is_24h" value="true" />
    <int name="azkar_min" value="10" />
    <long name="last_sync" value="1794794434005" />
    <float name="scale" value="1.25" />
    <set name="themes">
        <string>Theme 1</string>
        <string>Jamas</string>
    </set>
</map>"#;

        let prefs = parse_shared_prefs_xml(xml);
        assert_eq!(prefs.len(), 6);
        assert!(prefs.iter().any(|p| p.key == "masjid_name"
            && p.value_type == "string"
            && p.value == "Masjid Al-Hakim & Center"));
        assert!(prefs
            .iter()
            .any(|p| p.key == "is_24h" && p.value_type == "boolean" && p.value == "true"));
        assert!(prefs
            .iter()
            .any(|p| p.key == "azkar_min" && p.value_type == "int" && p.value == "10"));
        assert!(prefs.iter().any(|p| p.key == "themes"
            && p.value_type == "string_set"
            && p.value == r#"["Theme 1","Jamas"]"#));
    }

    fn pb_len_delim(field: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![((field << 3) | 2) as u8, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn decodes_datastore_preferences_pb() {
        // Entry 1: "theme" -> string("Jamas")
        let val1 = pb_len_delim(5, b"Jamas");
        let mut e1 = pb_len_delim(1, b"theme");
        e1.extend_from_slice(&pb_len_delim(2, &val1));

        // Entry 2: "onboarded" -> boolean(true)
        let val2 = vec![(1 << 3) as u8, 1];
        let mut e2 = pb_len_delim(1, b"onboarded");
        e2.extend_from_slice(&pb_len_delim(2, &val2));

        // Entry 3: "count" -> int(42)
        let val3 = vec![(3 << 3) as u8, 42];
        let mut e3 = pb_len_delim(1, b"count");
        e3.extend_from_slice(&pb_len_delim(2, &val3));

        let mut map_bytes = Vec::new();
        map_bytes.extend_from_slice(&pb_len_delim(1, &e1));
        map_bytes.extend_from_slice(&pb_len_delim(1, &e2));
        map_bytes.extend_from_slice(&pb_len_delim(1, &e3));

        let decoded = parse_preferences_pb(&map_bytes).unwrap();
        assert_eq!(
            decoded,
            vec![
                PrefEntry {
                    key: "count".into(),
                    value_type: "int".into(),
                    value: "42".into(),
                },
                PrefEntry {
                    key: "onboarded".into(),
                    value_type: "boolean".into(),
                    value: "true".into(),
                },
                PrefEntry {
                    key: "theme".into(),
                    value_type: "string".into(),
                    value: "Jamas".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn queries_local_sqlite_database_readonly() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!("android-lens-test-{stamp}.db"));

        let init = Command::new("sqlite3")
            .arg(&db)
            .arg("CREATE TABLE prayers(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO prayers VALUES (1, 'Fajr'), (2, 'Maghrib');")
            .status()
            .await
            .unwrap();
        assert!(init.success());

        let tables = sqlite_list_tables_local(&db).await.unwrap();
        assert_eq!(tables, vec!["prayers"]);

        let data = sqlite_query_local(&db, "test.db", "prayers", false, 50)
            .await
            .unwrap();
        assert_eq!(data.columns, vec!["id", "name"]);
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0], vec!["1", "Fajr"]);

        // Reject mutating queries
        let write_attempt =
            sqlite_query_local(&db, "test.db", "DELETE FROM prayers;", true, 50).await;
        assert!(write_attempt.is_err());

        let _ = std::fs::remove_file(&db);
    }
}
