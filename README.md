# The Raohe

---

專為 Android 系統打造的核心層級的高隱匿性 All-in-one 動態分析套件

- RT 驅動
    - 硬體中斷點
    - 虛擬觸控
- 核心層級隱匿式讀取機制
- 可選的 CPU 軟體渲染後端
- 防擷取疊加介面(SurfaceFlinger Layer)
- 已為 Unreal Engine(UE) 相關應用最佳化使用體驗
- 內建 Lua 腳本執行環境
- 支援 MCP 協定

---

## 建置、打包、部署

環境需求:
- Rust (`rustup target add aarch64-linux-android`)
- Android NDK (自動偵測, 或以 `ANDROID_NDK_HOME` / `--ndk` 指定)
- adb
- 裝置已對 Shell 授權 root (KernelSU / Magisk)

```bash
cargo xtask build --release   # 交叉編譯 Release
cargo xtask pack              # 打包 target/dist/theraohe.tar.gz (777, 含 CopyToRelease/ 內容)
cargo xtask deploy            # 建置 → 打包 → adb push → 解壓到 /data/local/tmp/theraohe/ → 執行
cargo xtask deploy --restart  # 先 pkill 再部署 (推薦)
```

## 預建置 Release 使用教程

前置:
- 裝置已取得 root (KernelSU / Magisk)
- 安裝 MT Manager (或任一支援 root 的檔案管理器)
- 瀏覽器下載 `theraohe.tar.gz` (預設落在 `/sdcard/Download/`)

1. MT Manager 開啟 `/sdcard/Download/theraohe.tar.gz`, 解壓到 `/data/local/tmp/` (需 root)
2. 以 root 身份執行 `/data/local/tmp/theraohe/Start.sh`
3. 螢幕上將出現疊加界面

不想用 MT Manager 的話, 也可以用 Termux:
    ```bash
    su
    tar -xzf /sdcard/Download/theraohe.tar.gz -C /data/local/tmp
    /data/local/tmp/theraohe/theraohe --show
    ```

已於 Google Pixel OS 17 (CP3A.261005.055)(KernelSU LKM) 測試。
