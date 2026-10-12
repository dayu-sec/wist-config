# 更新日志

## [0.1.0] - 2026-10-12

- 首个版本：抽取「主配置 + 现场态声明（`${VAR}` 引用）+ 同目录 `.env`」这一分层配置机制为通用 crate。
  - `load_layered_toml`：读主配置 + 现场态声明 → 「进程环境 > `.env`」插值 `${VAR}` / `${VAR:-默认}`
    （未定义且无默认 → 拒启）→ 无重复叶子合并。
  - `upsert_declared_vars` / `remove_declared_var`：按声明里的 `${VAR}` 名回写 / 删除 `.env` 行。
  - `LayerSpec`：文件名与覆盖用环境变量名由调用方给定，crate 不绑定任何组件的键名 / 文件名。
  - 错误复用 `wist-error` 的 `ConfigReason`（Io / Parse / Validation / General）。
