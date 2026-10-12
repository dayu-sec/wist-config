# 更新日志

## [0.1.0] - 2026-10-12

- 首个版本：抽取「主配置 + 现场态声明（`${VAR}` 引用）+ 同目录 `.env`」这一分层配置机制为通用 crate。
  - `load_layered_toml`：读主配置 + 现场态声明 → 「进程环境 > `.env`」插值 `${VAR}` / `${VAR:-默认}`
    （未定义且无默认 → 拒启）→ 无重复叶子合并。
  - `upsert_declared_vars` / `remove_declared_var`：按声明里的 `${VAR}` 名回写 / 删除 `.env` 行。
  - `LayerSpec`：文件名与覆盖用环境变量名由调用方给定，crate 不绑定任何组件的键名 / 文件名。
  - `overlay_path_with` / `values_path_with`：显式覆盖版（`None` = 约定兄弟名）——与进程环境解耦，
    嵌入方 / 测试可**确定性**指定落点。
  - 错误复用 `wist-error` 的 `ConfigReason`（Io / Parse / Validation / General）。
  - 测试：60 个单测，覆盖插值（`${VAR}` / `${VAR:-默认}` / 未闭合 / 空名 / 首个 `}` 收口 / 名 trim /
    值带冒号）、`.env` 解析与值写法（引号 / 转义 / 重复键 / `=` 与 `#` / 往返）、无重复叶子合并
    （嵌套表 / 表对叶子 / 数组 / 非表）、路径解析（默认兄弟名 / 相对与绝对覆盖）、整包加载
    （数组内插值 / 空 overlay / 无 overlay 不读 `.env` / 缺主配置报错）。
