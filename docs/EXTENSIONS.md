# 扩展包

扩展包是独立构建的 WebAssembly 模块（`extensions/` 工作区），按需下载到 `<配置目录>/ext/<id>/`，由内置解释器（wasmi）在沙箱中运行：不允许任何导入，限制指令预算与内存，每次调用都是新实例。

## ABI v1
- 导出：`memory`、`shellax_alloc(len: i32) -> i32`、`shellax_run(ptr: i32, len: i32) -> i64`（返回 `ptr << 32 | len`）。
- 缓冲区：`[u32 LE json 长度][json][附带字节]`。
- 请求：`{op: "manifest"|"run", lang, tool, args, files: [{field, name, len}], now_ms, utc_offset}`，文件内容按顺序放在附带字节里。
- 回复：`{text, error, color, file_name}`，输出文件放在附带字节里。
- 用 `shellax-ext-sdk` 的 `export!(manifest, run)` 即可。

## 现有扩展包
| id | 工具 | 体积（约） |
|---|---|---|
| lang-zh | 汉字转拼音、简繁转换 | 1.5 MB |
| worldtime | 世界时钟、时区换算 | 1.1 MB |
| image | 二维码生成/识别、条形码、图片压缩、图片水印 | 1.3 MB |
| pdf | 合并、拆分、旋转、删页、重排、水印、页码、图片转 PDF、提取文本、清理元数据 | 2.3 MB |
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

## 已知限制
- PDF 水印与页码使用标准 Helvetica 字体，暂不支持中日韩文字。
- 图片水印的内置点阵字体只含 ASCII；其他文字需选择字体文件。
