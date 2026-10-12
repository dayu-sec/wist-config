# wist-config

[![CI](https://github.com/dayu-sec/wist-config/actions/workflows/ci.yml/badge.svg)](https://github.com/dayu-sec/wist-config/actions)
[![crates.io](https://img.shields.io/crates/v/wist-config.svg)](https://crates.io/crates/wist-config)
[![docs.rs](https://docs.rs/wist-config/badge.svg)](https://docs.rs/wist-config)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](./LICENSE)

Layered TOML config for the wist ecosystem: a main file plus an optional *site overlay* that holds only
`${VAR}` references, with the values kept in a sibling `.env`.

## Why three layers

The same split is used by `wist-gateway`, `wist-center` and `wist-gwlinkd`:

- **Main config** is a *deliverable*: template-rendered, safe to overwrite on upgrade / re-render.
- **Site overlay** (e.g. `gwlinkd-pub.toml`) holds only `${VAR}` references — no site values — so it can be
  committed and overwritten on every upgrade.
- **Site values** (the `.env` next to the overlay, docker-compose style) hold the real values and are the
  single writer's output.

On load, the overlay is interpolated with **process env > sibling `.env`** and merged into the main config
(disjoint leaf keys). On write, the declared variable names drive which `.env` lines are updated.

## API

```rust
use wist_config::{LayerSpec, load_layered_toml, upsert_declared_vars, remove_declared_var};

const SPEC: LayerSpec = LayerSpec {
    overlay_file: "gwlinkd-pub.toml",
    overlay_env: "WIST_GWLINKD_PUB_CONFIG",
    values_file: ".env",
    values_env: "WIST_GWLINKD_ENV_FILE",
};

let doc = load_layered_toml(std::path::Path::new("gwlinkd.toml"), &SPEC)?;
```

- `load_layered_toml(main, spec)` — main + overlay → interpolated + merged `toml::Value`.
- `upsert_declared_vars(overlay, spec, kv)` / `remove_declared_var(overlay, spec, key)` — write back into the
  values file using the variable names declared in the overlay.
- Helpers: `overlay_path`, `resolve_overlay_path`, `values_path`, `load_vars`, `interpolate`,
  `interpolate_doc`, `merge_disjoint`, `declared_var_names`, `parse_env_lines`.

## Interpolation

- `${VAR}` and `${VAR:-default}`.
- **Undefined without a default is an error** (fail-closed): a forgotten `.env` never silently becomes empty.
- Precedence: process environment first, then the sibling `.env` (docker-compose semantics).

## License

Apache-2.0
