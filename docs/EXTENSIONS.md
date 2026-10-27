# 扩展包

扩展包是独立构建的 WebAssembly 模块（`extensions/` 工作区），按需下载到 `<配置目录>/ext/<id>/`，由内置解释器（wasmi）在沙箱中运行：不允许任何导入，限制指令预算与内存，每次调用都是新实例。

## ABI v1
- 导出：`memory`、`shellax_alloc(len: i32) -> i32`、`shellax_run(ptr: i32, len: i32) -> i64`（返回 `ptr << 32 | len`）。
- 缓冲区：`[u32 LE json 长度][json][附带字节]`。
- 请求：`{op: "manifest"|"run", lang, tool, args, files: [{field, name, len}], now_ms, utc_offset}`，文件内容按顺序放在附带字节里。
- 回复：`{text, error, color, file_name}`，输出文件放在附带字节里。
- 回复的可选字段（旧包不带也能用，仍按 `text` 显示）：`rows: [{label, value, primary, tag, hint, tone}]` 让结果按“每条一行”显示（`tone` 取 `good` / `warn` / `danger` / `accent` / `muted`，`primary` 为主结果，`tag` 为行尾小标签，`hint` 为悬停提示）；`meta` 为一行说明。有 `rows` 时 `text` 作为“复制全部”的文本（缺省由行拼出）。
- 文件结果的名称、大小、图片尺寸与预览、PDF 页数、压缩包文件数都由宿主从输出文件本身读出，包里不需要另外声明；图片压缩的“压缩前 → 压缩后”也由宿主用输入文件大小算出。因此现有扩展包无需重新打包。
- 用 `shellax-ext-sdk` 的 `export!(manifest, run)` 即可。

## 现有扩展包
| id | 工具 | 体积（约） |
|---|---|---|
| lang-zh | 汉字转拼音、简繁转换 | 1.5 MB |
| worldtime | 世界时钟、时区换算 | 1.1 MB |
| image | 二维码生成/识别、条形码、图片压缩、图片水印 | 约 2.4 MB |
| pdf | 合并、拆分、旋转、删页、重排、水印、页码、图片转 PDF、提取文本、清理元数据 | 约 4.4 MB |
| pdf-render | PDF 转图片（PNG，多页打包为 zip） | 5.3 MB |

## 构建与测试
```sh
cd extensions
cargo test
cargo build --release --target wasm32-unknown-unknown
```
构建出 wasm 后，主程序的 `cargo test ext::` 会在宿主运行时里实际加载这些包。

## 发布
`.github/workflows/ext-release.yml`：
- 手动触发默认 `dry_run = true`：只构建、生成 `index.json` 并作为构件上传，不发布。
- 推送 `ext-<id>-v<版本>` 标签：上传该包到同名 Release，并更新 `ext-index` Release 中的 `index.json`。
- 索引里每个包的下载地址指向 `ext-<id>-v<版本>`，因此每个包的当前版本都需要有对应标签。

## 中文字体
image 与 pdf 包内置 `extensions/assets/NotoSansSC-common.ttf`（Noto Sans SC 常用字子集，SIL OFL 1.1，约 1.1 MB）：
- PDF 水印 / 页码：纯西文仍用标准 Helvetica；含中文时按文档用到的字再次子集化，以 CID 字体（Type0 / CIDFontType2，Identity-H，带 ToUnicode）嵌入，每个文件只增加几 KB。
- 图片水印：纯 ASCII 用内置点阵字体，其他文字默认用内置中文字体，也可选择字体文件。
- 字库外的字（如生僻字、扩展区汉字）会直接报错并列出，不会画成空框。

重新生成子集（需要 fonttools）：
```sh
fonttools varLib.instancer NotoSansSC[wght].ttf wght=400 -o sc400.ttf
pyftsubset sc400.ttf --text-file=chars.txt --layout-features='' --no-hinting --name-IDs='*' --notdef-outline --output-file=NotoSansSC-common.ttf
```
