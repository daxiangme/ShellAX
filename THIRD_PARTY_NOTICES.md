# 第三方组件许可声明 / Third-party notices

ShellAX is licensed under AGPL-3.0 (see `LICENSE`). It includes or links the third-party components
listed below under their own licenses.

## 1. Source files containing third-party code

| File | Component | License | Copyright |
|---|---|---|---|
| `src/agent.rs` (items marked `[3p]`), modified | herdr | Apache-2.0 | Copyright (c) herdr contributors |
| `agents/claude.toml`, `agents/codex.toml`, `agents/grok.toml`, unmodified | herdr | Apache-2.0 | Copyright (c) herdr contributors |
| `src/layout.rs` (`auto_direction`, marked `[3p]`), modified | zellij | MIT | Copyright (c) 2020 Zellij contributors |

License texts: `licenses/herdr-LICENSE-Apache-2.0.txt`, `licenses/zellij-LICENSE-MIT.md`.

### herdr
Copyright (c) herdr contributors. Licensed under the Apache License, Version 2.0 (the "License");
you may not use these files except in compliance with the License. You may obtain a copy at
http://www.apache.org/licenses/LICENSE-2.0. Unless required by applicable law or agreed to in
writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
WARRANTIES OR CONDITIONS OF ANY KIND.

### zellij
MIT License — Copyright (c) 2020 Zellij contributors. The full permission notice is in
`licenses/zellij-LICENSE-MIT.md`.

## 2. Libraries linked as dependencies (not copied)

| Crate / asset | License |
|---|---|
| gpui-pre / gpui-pre-platform 0.3.9 (GPUI) | Apache-2.0 |
| alacritty_terminal 0.26.0 | Apache-2.0 |
| russh 0.64.1 (+ aws-lc-rs backend) | russh: Apache-2.0；aws-lc-rs: "ISC AND (Apache-2.0 OR ISC)"；aws-lc-sys: ISC AND Apache-2.0 AND MIT AND BSD-3-Clause 等组合（发布前需单独核对） |
| portable-pty 0.9.0 | MIT |
| gpui-component 0.7.3, gpui-base 0.7.3, gpui-component-macros 0.7.3 (settings inputs / switches) | Apache-2.0 |
| gpui-kit-assets 0.7.3 (embedded Lucide icon SVGs) | crate Apache-2.0; icons ISC — Copyright (c) Lucide Icons and Contributors (crate ships `LICENSE-LUCIDE`) |
| sha2 0.10, chrono 0.4, serde_json 1 (audit log) | MIT OR Apache-2.0 |
| regex 1.x, serde, toml, tokio, flume, anyhow, libc | MIT OR Apache-2.0 |
| keyring 3.6.3 (OS keychain; features async-secret-service + crypto-rust on Linux, apple-native, windows-native) | MIT OR Apache-2.0 |
| secret-service 4.0.0, zbus 4.4 (pure-Rust D-Bus, pulled in by keyring) | secret-service: MIT OR Apache-2.0; zbus: MIT |
| chacha20poly1305 0.10.1 (encrypted fallback file for API keys), rand 0.8 | Apache-2.0 OR MIT |
| ureq 2.12.1 (AI HTTP client) | MIT OR Apache-2.0 |
| rustls 0.23 (via ureq "tls") | Apache-2.0 OR ISC OR MIT |
| socks 0.3.4 (via ureq "socks-proxy"; SOCKS5 for AI/update requests) | MIT OR Apache-2.0 |
| serde_yaml 0.9.34 (managed-policy.yaml) | MIT OR Apache-2.0 |
| unsafe-libyaml 0.2.11 (via serde_yaml) | MIT |
| ksni 0.3.6 (Linux tray, StatusNotifierItem) | Unlicense (public-domain dedication; permissive) |
| zbus 4.4 / 5.19 (D-Bus, via ksni; zbus 5 also as dev-dependency for the test watcher) | MIT |
| pastey 0.2.3 (via ksni) | MIT OR Apache-2.0 |
| ring 0.17 (via rustls) | Apache-2.0 AND ISC (`ring` ships its LICENSE incl. the BoringSSL/OpenSSL-derived ISC parts; reproduce it in release notices) |
| webpki-roots 0.26 / 1.0 (Mozilla CA bundle, via ureq) | CDLA-Permissive-2.0 (data license; permissive, attribution — include the license text in releases) |
| Catppuccin Mocha / Latte palettes (`themes/catppuccin-*.toml`) | MIT, © Catppuccin |

Transitive notes: `self_cell` and `r-efi` are dual/tri-licensed and used under Apache-2.0/MIT; `dwrote` (Windows only), `cbindgen` (build tool) and `option-ext` (build-time, via rust-embed's proc-macro) are MPL-2.0 — not modified, no source copied.


Fonts are **not** bundled (see README "字体许可").

## models.dev（运行时数据）

- 来源：https://github.com/sst/models.dev ，许可证 MIT（Copyright (c) sst）。
- 用法：程序运行时从 https://models.dev/api.json 拉取模型元数据（上下文长度、价格），缓存在本地数据目录 24 小时。没有复制代码，也没有把数据打包进程序。如果以后改为打包快照，需要随附 MIT 许可文本。

## tree-sitter / tree-sitter-bash（依赖）

- tree-sitter 0.25（MIT，Copyright (c) 2018 Max Brunsfeld）、tree-sitter-bash 0.25.1（MIT，Copyright (c) 2017 Max Brunsfeld）、tree-sitter-language（MIT）。作为 crate 依赖链接（含 C 解析器源码，编译进二进制），没有复制代码。用于给命令分段，再做风险分级。

## notify-rust 4.x — MIT OR Apache-2.0 (dependency, unmodified)
## russh-sftp 3.0.1 — Apache-2.0 (dependency, unmodified)
## getrandom 0.4 — MIT OR Apache-2.0 (dependency, unmodified)
## toml_edit 0.22 — MIT OR Apache-2.0 (dependency, unmodified)

## image 0.25 — MIT OR Apache-2.0 (dependency, unmodified; decodes Kitty PNG payloads and builds gpui image frames; already a transitive dependency of gpui)


## gpui-pre-macos 0.3.9 — Apache-2.0 (vendored and modified: `vendor/gpui-pre-macos`)
- Source: crates.io `gpui-pre-macos` 0.3.9 (GPUI backend), Apache License 2.0. `LICENSE-APACHE` is in the vendored directory.
- Modification (2026-10-21, ShellAX): in `src/pasteboard.rs` `write_plaintext`, plain text is written with `setString:forType:` instead of `setData:forType:`. The original call stored raw UTF-8 bytes; other applications read that pasteboard type as MacRoman, so non-ASCII text pasted as mojibake. `setData:forType:` remains only as a fallback when `setString:forType:` returns false. A regression test checks that `stringForType:` returns the original Unicode. Nothing else was changed. The change is marked in the file with an "ShellAX patch" comment.
## gpui-pre-windows 0.3.9 — Apache-2.0 (vendored and modified: `vendor/gpui-pre-windows`)
- Source: crates.io `gpui-pre-windows` 0.3.9 (GPUI Windows backend), Apache License 2.0. `LICENSE-APACHE` is in the vendored directory.
- Modification (2026-10-14, ShellAX): in `build.rs`, when cross-compiling for Windows from a non-Windows host, copy `prebuilt/shaders_bytes.rs` into OUT_DIR. The original build script compiles the HLSL with fxc.exe only on a Windows host. The prebuilt file is generated by `tools/gen_win_shaders.sh` from the crate's own `src/*.hlsl`, using Microsoft fxc.exe under wine with the same entry points, profiles and flags. Nothing else was changed. The change is marked in the file with an "ShellAX patch" comment.
- fxc.exe / d3dcompiler_47.dll (Microsoft Windows SDK, NuGet package Microsoft.Windows.SDK.CPP) are build tools only and are not redistributed. Only the compiled shader bytecode ends up in the binary.
## windows-sys 0.61 — MIT OR Apache-2.0 (dependency, unmodified; Windows only: redirects stderr to the log file)
## sysinfo 0.31 — MIT (dependency, unmodified; local CPU / memory / disk / process / network readings on Windows and Mac; already a transitive dependency). Copyright (c) Guillaume Gomez.
## App icon (packaging/windows/shellax.ico) — original artwork for this project, generated with ImageMagick
## Symbols Nerd Font Mono (assets/fonts/SymbolsNerdFontMono-Regular.ttf) — MIT (bundled, unmodified)
Nerd Fonts v3.4.0 "NerdFontsSymbolsOnly" release, Copyright (c) 2014 Ryan L McIntyre. The release ships under the MIT
license (full text: `assets/fonts/SymbolsNerdFont-LICENSE.txt`). It is embedded in the binary and registered at startup as
a fallback font for prompt icons (powerline, devicons, etc.). The icon sets it aggregates are credited upstream
(https://github.com/ryanoasis/nerd-fonts#license): Font Awesome (OFL-1.1), Material Design Icons (Apache-2.0),
Octicons (MIT), Codicons (CC-BY-4.0), Powerline symbols (MIT), Font Logos (Unlicense), Pomicons (OFL-1.1), Weather Icons (OFL-1.1).
On Windows the default fallback chain also lists the system fonts "Segoe UI Symbol", "Segoe Fluent Icons" and "Segoe MDL2 Assets" (not bundled).
## sqlx 0.9 (sqlx-core / sqlx-mysql / sqlx-postgres / sqlx-sqlite) — MIT OR Apache-2.0 (dependency, unmodified; database drivers)
## libsqlite3-sys (bundled SQLite) — MIT; SQLite itself is public domain (dependency, unmodified)
## futures-util 0.3 — MIT OR Apache-2.0 (dependency, unmodified)
## redis 1.8 (redis-rs) — BSD-3-Clause (dependency, unmodified; Redis client). Copyright (c) 2022 by redis-rs contributors; redistribution must retain this notice and the license conditions.
## bollard 0.21 / bollard-stubs — Apache-2.0 (dependency, unmodified; Docker Engine API client)
## tiberius 0.13 — MIT OR Apache-2.0 (dependency, unmodified; SQL Server TDS client)
## tokio-util 0.7 — MIT (dependency, unmodified)
## mongodb 3 — Apache-2.0; bson 2 — MIT (dependencies, unmodified; MongoDB client)
## dm-conn 0.1.0 / dm-pool 0.1.0 — Apache-2.0 (dependencies, unmodified; async DM8 client and connection pool)
- Source: https://github.com/daxiangme/dm-rust revision 5a25bdf954686f0462d9bfbf154a95966efeb52e (crates `dm-conn`, `dm-pool`, and the internal `dm-proto` crate they build). License: Apache License 2.0. The upstream LICENSE file does not name a copyright holder.
## rustls 0.23 — Apache-2.0 OR ISC OR MIT (dependency, unmodified; TLS crypto provider selection)
## duckdb 1.x / libduckdb-sys (bundled DuckDB, optional `duckdb` build feature) — MIT; DuckDB itself MIT, Copyright (c) DuckDB Foundation (dependency, unmodified)
## lsp-types 0.97 — MIT (dependency, unmodified; completion item types for the SQL editor)
## 内置工具模块（`src/tools/`）依赖 — dependencies, unmodified
- md-5 0.10, sha1 0.10, hmac 0.12, digest 0.10, aes 0.8, des 0.8, rabbit 0.4, cipher 0.4, cbc 0.1, ecb 0.1, ctr 0.9, cfb-mode 0.8, ofb 0.6 (RustCrypto) — MIT OR Apache-2.0
- base64 0.22, percent-encoding 2, hex 0.4, uuid 1 — MIT OR Apache-2.0
- sqlformat 0.5 — MIT OR Apache-2.0; croner 4 — MIT; pulldown-cmark 0.13 — MIT
- `src/tools/pinyin.rs` 首字母表依据 GB2312 一级汉字的拼音排序（标准字符集顺序）生成，未复制第三方代码或数据库。
## tokio-tungstenite 0.28 / tungstenite 0.28 — MIT OR Apache-2.0 (dependency, unmodified; WebSocket 测试工具)
- 工具箱图标 assets/icons/db/toolbox.svg — 本项目原创图形
## wasmi 1.x / wasmi_core / wasmi_ir / wasmi_collections — MIT OR Apache-2.0；wasmparser — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT；string-interner — MIT OR Apache-2.0 (dependencies, unmodified; 扩展包 WebAssembly 解释器)
## wat / wast / wasm-encoder / leb128fmt — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (仅开发依赖，用于单元测试，不随程序分发)
## 扩展包（`extensions/`，独立 WebAssembly 包，仅在用户安装对应扩展时下载，不随主程序分发） — dependencies, unmodified
- 公共：serde_json — MIT OR Apache-2.0；crc32fast 1 — MIT OR Apache-2.0；getrandom 0.3/0.4 — MIT OR Apache-2.0
- 中文语言包（lang-zh）：pinyin 0.11 — MIT；hanconv 0.5 — MIT（内含简繁转换词典数据，Apache-2.0）
- 世界时间（worldtime）：chrono 0.4、chrono-tz 0.10 — MIT OR Apache-2.0（时区数据来自 IANA tz database，公有领域）
- 图片与条码（image）：image 0.24/0.25 及其编解码器（png、gif、jpeg-decoder、zune-jpeg、image-webp、tiff、exr 等）— MIT OR Apache-2.0 / BSD-3-Clause；qrcode 0.14 — MIT OR Apache-2.0；barcoders 2 — MIT OR Apache-2.0；bardecoder 0.5 — MIT；ab_glyph 0.2 / owned_ttf_parser — Apache-2.0；font8x8 0.3 — MIT
- PDF 工具（pdf）：lopdf 0.46 — MIT；encoding_rs — (Apache-2.0 OR MIT) AND BSD-3-Clause；stringprep / unicode-normalization — MIT OR Apache-2.0（含 Unicode-3.0 数据）
- PDF 渲染（pdf-render）：hayro 0.8 / hayro-interpret / hayro-syntax / hayro-jpeg2000 / hayro-jbig2 / hayro-ccitt / hayro-cmap / hayro-postscript、vello_cpu、vello_common、kurbo、peniko、skrifa、read-fonts、fearless_simd — Apache-2.0 OR MIT；pic-scale、moxcms、pxfm — BSD-3-Clause OR Apache-2.0；yoke / zerofrom — Unicode-3.0
  - 内嵌标准字体（.pfb）：Copyright 2014 PDFium Authors，原始代码 Copyright 2014 Foxit Software Inc.，BSD-3-Clause；内嵌 CMYK 配置文件 — CC0-1.0
- 中文字体（image、pdf 扩展包内置）：`extensions/assets/NotoSansSC-common.ttf`，由 Noto Sans SC（Copyright 2014-2021 Adobe，Reserved Font Name 'Source'）裁剪为常用字子集（ASCII、常用中文标点与 GB2312 一级汉字 3755 字，常规字重），SIL Open Font License 1.1，许可全文见 `extensions/assets/NotoSansSC-OFL.txt`；PDF 中按文档再次子集化嵌入
- PDF 中文字体嵌入（pdf）：subsetter 0.2 — MIT OR Apache-2.0；ttf-parser 0.25 — MIT OR Apache-2.0
- 完整传递依赖清单可在 `extensions/` 下运行 `cargo tree --target wasm32-unknown-unknown -e normal` 查看；均为 MIT / Apache-2.0 / BSD / Zlib / Unicode-3.0 / CC0 等宽松许可。
