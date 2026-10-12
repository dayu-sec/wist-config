//! 分层 TOML 配置：**主配置** + **现场态声明**（只写 `${VAR}` 引用）+ **同目录 `.env`**（真值）。
//!
//! 与 `wist-gateway` / `wist-center` / `wist-gwlinkd` 共用的分文件口径一致，一份配置拆三层：
//!   - **主配置**是**交付物**：模板渲染、升级 / 重渲该被覆盖；
//!   - **现场态声明**（overlay，如 `gwlinkd-pub.toml`）**只写引用**：
//!     `control_center_endpoint = "${CENTER_ENDPOINT}"` —— 不含任何现场值，因此**可以入库、可以随版本覆盖**；
//!   - **现场态值**（values，与声明同目录的 `.env`，docker-compose 口径）：真值写这里，**唯一写者**。
//!
//! 加载时（[`load_layered_toml`]）：读主配置 + 读现场态声明 → 用「**进程环境 > 同目录 `.env`**」插值 `${VAR}`
//! （未定义且无 `:-默认值` → **拒启**，fail-closed）→ 两份合并（段可重合、**叶子键不许重复**）。
//!
//! 回写时（[`upsert_declared_vars`] / [`remove_declared_var`]）：写哪个变量名由**现场态声明**决定
//! （声明里 `gateway_id = "${GATEWAY_ID}"` ⇒ 写 `GATEWAY_ID=…`）—— 于是「配置键 → 变量名」的映射只有一处，
//! 部署方可自由选名，而声明随版本重渲也动不到现场值。
//!
//! 具体约定（文件名 / 覆盖用的环境变量名 / 哪个文件当第三层）由调用方通过 [`LayerSpec`] 给出 —— 本 crate
//! 不绑定任何组件的键名与文件名。

mod envfile;
mod layer;

pub use envfile::{declared_var_names, parse_env_lines, remove_declared_var, upsert_declared_vars};
pub use layer::{
    LayerSpec, interpolate, interpolate_doc, load_layered_toml, load_vars, merge_disjoint,
    overlay_path, resolve_overlay_path, values_path,
};

pub use wist_error::{ConfigError, ConfigReason, ConfigResult};

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    use crate::LayerSpec;

    /// 测试用的分层约定：与任何真实组件解耦的专用文件名 / 环境变量名。
    pub const SPEC: LayerSpec = LayerSpec {
        overlay_file: "app-pub.toml",
        overlay_env: "WIST_CONFIG_TEST_PUB_CONFIG",
        values_file: ".env",
        values_env: "WIST_CONFIG_TEST_ENV_FILE",
    };

    /// 每个测试独立的临时目录（按 tag + pid 命名，跑前清空）。
    pub fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wist-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
