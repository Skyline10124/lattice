# 更纱黑体 UI SC 网页字体

网页端使用的更纱黑体 UI SC 常规体（Sarasa UI SC Regular），按 Unicode 区段切成 97 个 woff2 小文件，页面只下载实际显示的字。
仓库只保存 `sarasa-ui-sc.css`（各切片的 `@font-face` 声明）和字体许可 `OFL.txt`，不提交切片本身。

## 获取切片

切片取自 npm 包 `subsetted-fonts@1.0.5` 的 `SarasaUiSC-Regular/` 目录，文件名与 `sarasa-ui-sc.css` 中引用的一致（`SarasaUiSC-Regular.0.woff2` 至 `SarasaUiSC-Regular.96.woff2`）。在本目录下执行：

```sh
for i in $(seq 0 96); do
  curl -fsSLO "https://cdn.jsdelivr.net/npm/subsetted-fonts@1.0.5/SarasaUiSC-Regular/SarasaUiSC-Regular.$i.woff2"
done
```

页面加载 `sarasa-ui-sc.css` 即可；已安装更纱黑体的系统会优先使用本地字体。GPUI 等原生界面不使用这些切片，改为随应用打包完整字体文件，见设计系统 README 的“在 GPUI 中使用”。

## 许可

字体以 SIL Open Font License 1.1 发布，全文见 `OFL.txt`。
