//! 现场态**值文件**（`.env`）的读写：解析、按「声明里的变量名」回写、删除。
//!
//! 回写的落点与变量名都由**现场态声明**决定（见 crate 文档）：声明里
//! `gateway_id = "${GATEWAY_ID}"` ⇒ 写 `GATEWAY_ID=…`。于是「配置键 → 变量名」的映射只有一处。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orion_error::conversion::ToStructError;
use orion_error::prelude::*;

use wist_error::{ConfigReason, ConfigResult};

use crate::LayerSpec;
use crate::layer::values_path;

/// 解析 `.env` 风格的行：`KEY=VALUE`（`#` 开头 / 空行跳过；值两侧可选引号）。
///
/// **键名大小写敏感**（变量名就是大小写敏感的）；不报错、跳过不成形的行
/// （它是**值来源**，一个多余字符不该让进程起不来）。
pub fn parse_env_lines(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), strip_optional_quotes(value.trim())));
    }
    out
}

/// 现场态声明里的「配置键 → 变量名」表：只收**整串就是引用**的值
/// （`${VAR}` / `${VAR:-默认}`）；字面量不算（它不需要变量，也就没有可写的名字）。
/// 声明不存在 / 不可解析 → 空表（调用方决定是报错还是跳过）。
pub fn declared_var_names(overlay: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(overlay) else {
        return HashMap::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
        return HashMap::new();
    };
    let Some(table) = doc.as_table() else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (key, value) in table {
        if let Some(name) = value.as_str().and_then(var_ref_name) {
            out.insert(key.clone(), name);
        }
    }
    out
}

/// 把若干「配置键 → 值」按**现场态声明**里的变量名 upsert 进现场态值文件（`.env`）。
///
/// 声明缺失 → 报错（不知道写哪个变量名，宁可拒绝也不写一个无用键）。
/// 返回**未被声明**（写不进去）的键，供调用方提示「声明里没有该键的 `${VAR}` 引用」。
pub fn upsert_declared_vars(
    overlay: &Path,
    spec: &LayerSpec,
    kv: &[(&str, &str)],
) -> ConfigResult<Vec<String>> {
    if !overlay.is_file() {
        return Err(ConfigReason::Io.to_err().with_detail(format!(
            "现场态声明缺失 {} —— 无法把现场值落盘（它决定“哪个变量提供哪个键”）；\
             请先让安装脚本 / init-config 生成它",
            overlay.display()
        )));
    }
    let names = declared_var_names(overlay);
    let values = values_path(overlay, spec);
    let original = read_or_empty(&values)?;
    let mut text = original.clone();
    let mut undeclared = Vec::new();
    for (key, value) in kv {
        match names.get(*key) {
            Some(name) => text = upsert_env_line(&text, name, value),
            None => undeclared.push((*key).to_string()),
        }
    }
    if text != original {
        write_atomic(&values, &text)?;
    }
    Ok(undeclared)
}

/// 从现场态值文件（`.env`）里**删掉**某个配置键对应的一行。
/// 幂等：没有该行 / **文件不存在** / **键没被声明** → 返回 `false`，不动文件。
pub fn remove_declared_var(overlay: &Path, spec: &LayerSpec, key: &str) -> ConfigResult<bool> {
    let values = values_path(overlay, spec);
    if !values.is_file() {
        return Ok(false);
    }
    let Some(name) = declared_var_names(overlay).get(key).cloned() else {
        return Ok(false);
    };
    let text = read_or_empty(&values)?;
    let (out, removed) = remove_env_line(&text, &name);
    if removed {
        write_atomic(&values, &out)?;
    }
    Ok(removed)
}

/// 去掉值两侧成对的引号（`"…"` / `'…'`），并还原双引号里的转义。
fn strip_optional_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        return value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

/// 若整串就是 `${VAR}` / `${VAR:-…}`，返回 `VAR`。
fn var_ref_name(text: &str) -> Option<String> {
    let inner = text.strip_prefix("${")?.strip_suffix('}')?;
    if inner.contains('}') || inner.contains('$') {
        return None; // 不是“整串单个引用”
    }
    let name = inner
        .split_once(":-")
        .map_or(inner, |(name, _)| name)
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 读文件；不存在当空串（首次写入就是由它建出来）。
fn read_or_empty(path: &Path) -> ConfigResult<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(ConfigReason::Io
            .to_err()
            .with_detail(format!("读取 {} 失败: {err}", path.display()))),
    }
}

/// 把 `.env` 里的 `name=…` 换成新值（无则追加）；**保留其余行与注释**。
fn upsert_env_line(text: &str, name: &str, value: &str) -> String {
    let line = format!("{name}={}", env_value(value));
    let mut replaced = false;
    let mut out = String::new();
    for raw in text.lines() {
        if !replaced && env_line_key(raw) == Some(name) {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(raw);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// 从 `.env` 里删掉 `name=…` 那一行；返回（新内容，是否删掉了）。
fn remove_env_line(text: &str, name: &str) -> (String, bool) {
    let mut removed = false;
    let mut out = String::new();
    for raw in text.lines() {
        if env_line_key(raw) == Some(name) {
            removed = true;
            continue;
        }
        out.push_str(raw);
        out.push('\n');
    }
    (out, removed)
}

/// 该行若是 `KEY=…` 则返回 `KEY`（**大小写敏感**；注释与空行不算）。
fn env_line_key(raw: &str) -> Option<&str> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

/// `.env` 的值写法：普普通通的就不加引号；含空白 / `#` / 引号 / 反斜杠则加双引号并转义
/// （空值保持裸空：`KEY=`）。
fn env_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '#' | '"' | '\'' | '\\'))
    {
        return format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    }
    value.to_string()
}

/// 原子写：先写临时文件再 rename，避免写一半损坏配置。
fn write_atomic(path: &Path, content: &str) -> ConfigResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).source_err(
            ConfigReason::Io,
            format!("创建目录失败 {}", parent.display()),
        )?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content)
        .source_err(ConfigReason::Io, format!("写入失败 {}", tmp.display()))?;
    std::fs::rename(&tmp, path).source_err(ConfigReason::Io, format!("落盘失败 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{SPEC, temp_dir};

    /// 三个现场键都用变量引用的声明。
    const DECLARATION: &str = "endpoint = \"${APP_ENDPOINT}\"\nid = \"${APP_ID}\"\nca = \"${APP_CA:-}\"\ntoken = \"${APP_TOKEN:-}\"\n";

    fn write_declaration(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(SPEC.overlay_file);
        std::fs::write(&path, body).expect("write declaration");
        path
    }

    #[test]
    fn upsert_writes_the_declared_variables_into_the_values_file() {
        let dir = temp_dir("upsert");
        let overlay = write_declaration(&dir, DECLARATION);
        let undeclared = upsert_declared_vars(
            &overlay,
            &SPEC,
            &[
                ("endpoint", "https://center.example"),
                ("id", "gw-9"),
                ("token", "link_abc"),
                ("ca", "/ca.pem"),
            ],
        )
        .expect("upsert");
        assert!(undeclared.is_empty(), "{undeclared:?}");

        let env_text = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert!(
            env_text.contains("APP_ENDPOINT=https://center.example"),
            "{env_text}"
        );
        assert!(env_text.contains("APP_ID=gw-9"), "{env_text}");
        assert!(env_text.contains("APP_TOKEN=link_abc"), "{env_text}");
        assert!(env_text.contains("APP_CA=/ca.pem"), "{env_text}");
        // 声明文件**不被改**（它不含现场值）—— 这正是“重渲也动不到现场”的来源。
        let decl = std::fs::read_to_string(&overlay).expect("read declaration");
        assert_eq!(decl, DECLARATION, "声明不应被写入现场值");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_follows_the_declared_variable_names() {
        // 变量名由部署方定：声明写 `${GW_ENDPOINT}`，就写这个名。
        let dir = temp_dir("upsert-name");
        let overlay = write_declaration(
            &dir,
            "endpoint = \"${GW_ENDPOINT}\"\ntoken = \"${GW_TOKEN:-}\"\n",
        );
        upsert_declared_vars(
            &overlay,
            &SPEC,
            &[("endpoint", "https://c"), ("token", "link_x")],
        )
        .expect("upsert");
        let env_text = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert!(env_text.contains("GW_ENDPOINT=https://c"), "{env_text}");
        assert!(env_text.contains("GW_TOKEN=link_x"), "{env_text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_reports_keys_that_are_not_declared() {
        let dir = temp_dir("upsert-undeclared");
        let overlay = write_declaration(&dir, DECLARATION);
        let undeclared = upsert_declared_vars(
            &overlay,
            &SPEC,
            &[("endpoint", "https://c"), ("missing", "x")],
        )
        .expect("upsert");
        assert_eq!(undeclared, vec!["missing".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_preserves_other_env_lines_and_blanks() {
        let dir = temp_dir("upsert-keep");
        let overlay = write_declaration(&dir, DECLARATION);
        std::fs::write(
            values_path(&overlay, &SPEC),
            "# 现场自注\nAPP_ID=gw-old\nOTHER=keep-me\n",
        )
        .expect("write .env");
        upsert_declared_vars(&overlay, &SPEC, &[("endpoint", "https://c")]).expect("upsert");
        let env_text = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert!(env_text.contains("# 现场自注"), "{env_text}");
        assert!(env_text.contains("APP_ID=gw-old"), "不该动别人：{env_text}");
        assert!(env_text.contains("OTHER=keep-me"), "{env_text}");
        assert!(env_text.contains("APP_ENDPOINT=https://c"), "{env_text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_quotes_values_that_need_it_and_is_idempotent() {
        let dir = temp_dir("upsert-quote");
        let overlay = write_declaration(&dir, DECLARATION);
        upsert_declared_vars(&overlay, &SPEC, &[("id", "gw 1")]).expect("first");
        let once = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert!(once.contains("APP_ID=\"gw 1\""), "含空白应加引号：{once}");
        upsert_declared_vars(&overlay, &SPEC, &[("id", "gw 1")]).expect("second");
        let twice = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert_eq!(once, twice, "重复调用结果应稳定");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_requires_the_declaration_file() {
        // 没有声明就不知道写哪个变量名 —— 报错要可执行（而不是静默写一个无用键）。
        let dir = temp_dir("upsert-no-decl");
        let err = upsert_declared_vars(
            &dir.join(SPEC.overlay_file),
            &SPEC,
            &[("endpoint", "https://c")],
        )
        .expect_err("缺声明必须报错");
        assert!(err.to_string().contains("现场态声明缺失"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_declared_var_deletes_the_values_line() {
        let dir = temp_dir("remove");
        let overlay = write_declaration(&dir, DECLARATION);
        std::fs::write(
            values_path(&overlay, &SPEC),
            "# 头部\nAPP_ENDPOINT=https://c\nAPP_TOKEN=link_abc\nOTHER=keep\n",
        )
        .expect("write .env");
        assert!(remove_declared_var(&overlay, &SPEC, "token").expect("remove"));
        let env_text = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read .env");
        assert!(!env_text.contains("link_abc"), "应去掉券：{env_text}");
        assert!(env_text.contains("# 头部"), "注释应保留：{env_text}");
        assert!(env_text.contains("OTHER=keep"), "{env_text}");
        assert!(
            !remove_declared_var(&overlay, &SPEC, "token").expect("again"),
            "幂等"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_uses_the_declared_name_and_tolerates_a_missing_env() {
        let dir = temp_dir("remove-name");
        let overlay = write_declaration(&dir, "token = \"${GW_TOKEN:-}\"\n");
        // `.env` 不存在 → 幂等 false。
        assert!(!remove_declared_var(&overlay, &SPEC, "token").expect("missing env"));
        std::fs::write(values_path(&overlay, &SPEC), "# 头\nGW_TOKEN=link_abc\n").expect("write");
        assert!(remove_declared_var(&overlay, &SPEC, "token").expect("remove"));
        let env_text = std::fs::read_to_string(values_path(&overlay, &SPEC)).expect("read");
        assert!(!env_text.contains("link_abc"), "{env_text}");
        assert!(env_text.contains("# 头"), "{env_text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn declared_var_names_only_keeps_whole_references() {
        let dir = temp_dir("declared");
        let overlay = write_declaration(
            &dir,
            "endpoint = \"${APP_ENDPOINT}\"\nca = \"${APP_CA:-/default.pem}\"\nliteral = \"https://x\"\n",
        );
        let names = declared_var_names(&overlay);
        assert_eq!(
            names.get("endpoint").map(String::as_str),
            Some("APP_ENDPOINT")
        );
        assert_eq!(names.get("ca").map(String::as_str), Some("APP_CA"));
        assert!(!names.contains_key("literal"), "字面量没有可写的变量名");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn env_line_key_ignores_comments_blanks_and_non_assignments() {
        assert_eq!(env_line_key("TOKEN=abc"), Some("TOKEN"));
        assert_eq!(env_line_key("  TOKEN = abc  "), Some("TOKEN"));
        assert_eq!(env_line_key("# TOKEN=abc"), None);
        assert_eq!(env_line_key(""), None);
        assert_eq!(env_line_key("没有等号"), None);
    }

    #[test]
    fn parse_env_lines_strips_quotes_and_skips_noise() {
        let lines = parse_env_lines("# c\nA=1\nB=\"two words\"\nC='raw'\n\nnot-a-line\nD=a#b\n");
        assert_eq!(
            lines,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string()),
                ("C".to_string(), "raw".to_string()),
                ("D".to_string(), "a#b".to_string()),
            ]
        );
    }
}
