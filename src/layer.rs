//! 分层加载：路径约定 + 变量表 + `${VAR}` 插值 + 无重复叶子合并。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orion_error::conversion::ToStructError;
use orion_error::prelude::*;

use wist_error::{ConfigReason, ConfigResult};

/// 一套「分层配置」的命名与覆盖约定（本 crate 不绑定任何组件的键名 / 文件名）。
///
/// 三个约定名 + 两个「显式指定」的环境变量名，组件的配置模块只需给这几个常量。
#[derive(Debug, Clone, Copy)]
pub struct LayerSpec {
    /// **现场态声明**的文件名（与主配置同目录），如 `"gwlinkd-pub.toml"`。
    pub overlay_file: &'static str,
    /// 可显式指定现场态声明落点的环境变量，如 `"WIST_GWLINKD_PUB_CONFIG"`
    /// （相对路径按**主配置目录**解析）。
    pub overlay_env: &'static str,
    /// **现场态值**的文件名（与现场态声明同目录），如 `".env"`。
    pub values_file: &'static str,
    /// 可显式指定现场态值落点的环境变量，如 `"WIST_GWLINKD_ENV_FILE"`
    /// （相对路径按**现场态声明目录**解析）。
    pub values_env: &'static str,
}

/// 现场态**声明**的落点：显式 [`LayerSpec::overlay_env`] 优先（相对路径按主配置目录解析）；
/// 未设则取主配置同目录的约定文件名。
///
/// 与 [`resolve_overlay_path`] 的区别：本函数**不看文件是否存在** —— 靠它对现场态声明寻址
/// （做诊断 / 回写时都要算它）。
pub fn overlay_path(config_path: &Path, spec: &LayerSpec) -> PathBuf {
    match env_override(spec.overlay_env) {
        Some(raw) => absolutize(config_path, Path::new(&raw)),
        None => config_path.with_file_name(spec.overlay_file),
    }
}

/// 读侧：解析现场态声明的路径（**存在才用**；不存在 = 尚未接线，不报错）。
pub fn resolve_overlay_path(config_path: &Path, spec: &LayerSpec) -> Option<PathBuf> {
    let path = overlay_path(config_path, spec);
    path.is_file().then_some(path)
}

/// 现场态**值**（`.env`）的落点：与现场态**声明同目录**（docker-compose 口径：`.env` 挨着声明）；
/// 可用 [`LayerSpec::values_env`] 显式指定（相对路径按声明目录解析）。
pub fn values_path(overlay: &Path, spec: &LayerSpec) -> PathBuf {
    match env_override(spec.values_env) {
        Some(raw) => absolutize(overlay, Path::new(&raw)),
        None => overlay.with_file_name(spec.values_file),
    }
}

/// 读主配置 +（存在的话）现场态声明，插值 + 合并后返回解析好的 `toml::Value`。
///
/// 两份的键**不相交** —— 重复即拒（见 [`merge_disjoint`]）：
/// 「两处都写了、到底哪份说了算」不靠加载顺序，靠声明。
pub fn load_layered_toml(main: &Path, spec: &LayerSpec) -> ConfigResult<toml::Value> {
    let text = std::fs::read_to_string(main)
        .source_err(ConfigReason::Io, format!("读取配置失败 {}", main.display()))?;
    let mut doc: toml::Value = toml::from_str(&text).source_raw_err(
        ConfigReason::Parse,
        format!("解析配置失败 {}", main.display()),
    )?;
    if let Some(overlay) = resolve_overlay_path(main, spec) {
        let overlay_text = std::fs::read_to_string(&overlay).source_err(
            ConfigReason::Io,
            format!("读取现场态配置失败 {}", overlay.display()),
        )?;
        let mut overlay_doc: toml::Value = toml::from_str(&overlay_text).source_raw_err(
            ConfigReason::Parse,
            format!("解析现场态配置失败 {}", overlay.display()),
        )?;
        // 现场态**声明**里的值是 `${VAR}` 引用（不含真值）——
        // 用「进程环境 > 同目录 `.env`」插值（docker-compose 口径）。
        let vars = load_vars(&overlay, spec);
        interpolate_doc(&mut overlay_doc, &vars, &overlay, spec)?;
        merge_disjoint(&mut doc, overlay_doc, &overlay)?;
    }
    Ok(doc)
}

/// 合成插值用的变量表：**先 `.env`、后进程环境**（后者赢）—— 与 docker-compose 同口径
/// （shell 里已有的变量优先于 `.env`）。`.env` 不存在 = 空表（不是错误）。
pub fn load_vars(overlay: &Path, spec: &LayerSpec) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(values_path(overlay, spec)) {
        for (key, value) in super::parse_env_lines(&text) {
            vars.insert(key, value);
        }
    }
    for (key, value) in std::env::vars_os() {
        if let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) {
            vars.insert(key, value);
        }
    }
    vars
}

/// 递归插值：把文档里所有**字符串**值里的 `${VAR}` 换成变量值。
pub fn interpolate_doc(
    doc: &mut toml::Value,
    vars: &HashMap<String, String>,
    overlay: &Path,
    spec: &LayerSpec,
) -> ConfigResult<()> {
    match doc {
        toml::Value::String(text) => *text = interpolate(text, vars, overlay, spec)?,
        toml::Value::Table(table) => {
            for (_, value) in table.iter_mut() {
                interpolate_doc(value, vars, overlay, spec)?;
            }
        }
        toml::Value::Array(items) => {
            for value in items.iter_mut() {
                interpolate_doc(value, vars, overlay, spec)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 单串插值：支持 `${VAR}` 与 `${VAR:-默认值}`。
///
/// **未定义且无默认值 → 报错**（fail-closed：不让“忘了写 `.env`”静默变空，到远处才炸）。
/// 已定义但为空 = 就用空值（不取默认值）。
pub fn interpolate(
    text: &str,
    vars: &HashMap<String, String>,
    overlay: &Path,
    spec: &LayerSpec,
) -> ConfigResult<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(ConfigReason::Parse.to_err().with_detail(format!(
                "现场态配置 {} 的 `${{` 没有闭合：{text}",
                overlay.display()
            )));
        };
        let expr = &after[..end];
        let (name, default) = match expr.split_once(":-") {
            Some((name, default)) => (name.trim(), Some(default)),
            None => (expr.trim(), None),
        };
        match (vars.get(name), default) {
            (Some(value), _) => out.push_str(value),
            (None, Some(default)) => out.push_str(default),
            (None, None) => {
                return Err(ConfigReason::Parse.to_err().with_detail(format!(
                    "现场态配置 {} 引用了未定义的环境变量 `{name}`（也没给 `:-默认值`）—— \
                     请在进程环境或同目录的 {} 里给它赋值",
                    overlay.display(),
                    spec.values_file
                )));
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 把现场态配置 `overlay` 并进主配置 `base`。
///
/// - 段（TOML table）可以两份都写 —— 各自的键各写各的；
/// - **叶子键不允许重复**：同一个键在两份里都出现 → 直接报错。
///
/// 这是「归属唯一」的硬保证：否则“两处都写了、到底哪份说了算”就靠加载顺序决定，
/// 而那正是分文件要消灭的东西。
pub fn merge_disjoint(
    base: &mut toml::Value,
    overlay: toml::Value,
    overlay_path: &Path,
) -> ConfigResult<()> {
    let (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) = (base, overlay)
    else {
        return Err(ConfigReason::Parse.to_err().with_detail(format!(
            "现场态配置 {} 与主配置都必须是 TOML 表",
            overlay_path.display()
        )));
    };
    for (key, overlay_value) in overlay_table {
        if !base_table.contains_key(&key) {
            base_table.insert(key, overlay_value);
            continue;
        }
        let base_value = base_table.get_mut(&key).expect("checked contains_key");
        if base_value.is_table() && overlay_value.is_table() {
            merge_disjoint(base_value, overlay_value, overlay_path)?;
            continue;
        }
        return Err(ConfigReason::Validation.to_err().with_detail(format!(
            "键 `{key}` 同时出现在主配置与现场态配置 {} —— 同一个键只能有一处，请只留一份。\n       \
             现场选择（随部署而变、升级该保留的键）应只在现场态配置里；\n       \
             若这是升级前的老现场，把主配置里这几行删掉（值若与现场态不同，先手工合过去）。",
            overlay_path.display()
        )));
    }
    Ok(())
}

/// 读环境变量并 `trim`；未设 / 去空白后为空 → `None`。
fn env_override(name: &str) -> Option<String> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

/// 相对路径按**给定文件所在目录**解析；已是绝对路径则原样返回。
fn absolutize(anchor_file: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let dir = anchor_file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{SPEC, temp_dir};

    /// 写主配置 + （可选）现场态声明 + （可选）`.env`；返回**主配置路径**。
    fn write_split(
        dir: &Path,
        main_body: &str,
        overlay: Option<&str>,
        env_lines: Option<&str>,
    ) -> PathBuf {
        let main = dir.join("app.toml");
        std::fs::write(&main, main_body).expect("write main");
        if let Some(body) = overlay {
            std::fs::write(dir.join(SPEC.overlay_file), body).expect("write overlay");
        }
        if let Some(lines) = env_lines {
            std::fs::write(dir.join(SPEC.values_file), lines).expect("write values");
        }
        main
    }

    const DECLARATION: &str =
        "endpoint = \"${APP_ENDPOINT}\"\nid = \"${APP_ID}\"\nca = \"${APP_CA:-}\"\n";

    #[test]
    fn loads_the_site_values_through_the_declaration_and_the_env_file() {
        let dir = temp_dir("load-ok");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\nself_endpoint = \"https://127.0.0.1\"\n",
            Some(DECLARATION),
            Some("APP_ENDPOINT=https://center.example\nAPP_ID=gw-9\nAPP_CA=/ca.pem\n"),
        );
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        assert_eq!(doc["endpoint"].as_str(), Some("https://center.example"));
        assert_eq!(doc["id"].as_str(), Some("gw-9"));
        assert_eq!(doc["ca"].as_str(), Some("/ca.pem"));
        assert_eq!(doc["self_endpoint"].as_str(), Some("https://127.0.0.1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_absent_declaration_leaves_the_main_config_alone() {
        let dir = temp_dir("load-none");
        let main = write_split(&dir, "state_dir = \"/s\"\n", None, None);
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        assert_eq!(doc["state_dir"].as_str(), Some("/s"));
        assert!(doc.get("endpoint").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_env_file_only_works_through_the_defaults() {
        // `:-` 默认值让“还没写 .env”也能起（可选键）；无默认值的键则必须给值。
        let dir = temp_dir("load-defaults");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\n",
            Some("ca = \"${APP_CA:-}\"\n"),
            None,
        );
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        assert_eq!(doc["ca"].as_str(), Some(""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_undefined_variable_without_a_default_is_rejected() {
        // fail-closed：不让“忘了写 .env”静默变空。
        let dir = temp_dir("load-undefined");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\n",
            Some("endpoint = \"${WIST_CONFIG_TEST_UNDEFINED}\"\n"),
            Some("OTHER=1\n"),
        );
        let err = load_layered_toml(&main, &SPEC).expect_err("未定义变量必须报错");
        assert!(err.to_string().contains("未定义的环境变量"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_process_env_wins_over_the_env_file() {
        // docker-compose 口径：shell 里已有的变量优先于 `.env`。
        let dir = temp_dir("load-priority");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\n",
            Some("id = \"${WIST_CONFIG_TEST_PRIORITY}\"\n"),
            Some("WIST_CONFIG_TEST_PRIORITY=from-env-file\n"),
        );
        // SAFETY: 单测内设置一个**专用**变量名，不与其它测试的变量相交。
        unsafe { std::env::set_var("WIST_CONFIG_TEST_PRIORITY", "from-process-env") };
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        unsafe { std::env::remove_var("WIST_CONFIG_TEST_PRIORITY") };
        assert_eq!(doc["id"].as_str(), Some("from-process-env"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_values_file_lives_beside_the_declaration() {
        let dir = temp_dir("load-values-path");
        let overlay = dir.join(SPEC.overlay_file);
        assert_eq!(values_path(&overlay, &SPEC), dir.join(SPEC.values_file));
        // 显式指定时跟显式的走。
        unsafe { std::env::set_var(SPEC.values_env, "/tmp/other.env") };
        let explicit = values_path(&overlay, &SPEC);
        unsafe { std::env::remove_var(SPEC.values_env) };
        assert_eq!(explicit, PathBuf::from("/tmp/other.env"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_same_leaf_key_in_both_files_is_rejected() {
        let dir = temp_dir("load-dup");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\nself_endpoint = \"https://a\"\n",
            Some("self_endpoint = \"https://b\"\n"),
            None,
        );
        let err = load_layered_toml(&main, &SPEC).expect_err("同一叶子键出现在两份里必须报错");
        assert!(err.to_string().contains("同一个键只能有一处"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_declared_key_that_is_a_literal_is_loaded_verbatim() {
        // 声明里也可以写**字面量**（不需要变量）—— 那种键就不参与 `.env` 回写。
        let dir = temp_dir("load-literal");
        let main = write_split(
            &dir,
            "state_dir = \"/s\"\n",
            Some("id = \"gw-literal\"\nca = \"/ca.pem\"\n"),
            None,
        );
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        assert_eq!(doc["id"].as_str(), Some("gw-literal"));
        assert_eq!(doc["ca"].as_str(), Some("/ca.pem"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tables_in_both_files_merge_by_leaf() {
        let dir = temp_dir("load-tables");
        let main = write_split(
            &dir,
            "[server]\nlisten = \"127.0.0.1:80\"\n",
            Some("[agent]\ntenant_id = \"${APP_TENANT}\"\n"),
            Some("APP_TENANT=tenant-1\n"),
        );
        let doc = load_layered_toml(&main, &SPEC).expect("load");
        assert_eq!(doc["server"]["listen"].as_str(), Some("127.0.0.1:80"));
        assert_eq!(doc["agent"]["tenant_id"].as_str(), Some("tenant-1"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
