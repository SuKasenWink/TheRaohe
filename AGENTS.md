# Repository Guidelines

## Key Points
1. 隱匿性至上, 存在更隱蔽的方法可選就必須選擇
2. 協程 > 非同步線程
3. 偏好原生樣式, 不覆寫

## Building and Testing

```bash
cargo xtask build --release    # 交叉編譯 aarch64-linux-android (自動偵測 NDK)
cargo xtask deploy             # 打包 tar.gz (777, 二進位 + CopyToRelease/ 內容) -> adb push -> 覆蓋解壓到 /data/local/tmp/theraohe/ -> 執行
cargo xtask deploy --restart   # 先 pkill theraohe 再部署(推薦)
cargo check --target aarch64-linux-android
```

## MCP
- 自帶 MCP server, 綁 `127.0.0.1:8080`, 僅本機直連, 無認證
- 電腦端測試: `adb forward tcp:8080 tcp:8080` 後打 `http://127.0.0.1:8080`
- 不綁 0.0.0.0, 不暴露 LAN; 鑒權暫定維持 forward 模式 (不做靜態 Bearer, 不嵌 OAuth 2.1)
- 之後要 auth: 優先靜態 Bearer + 設置頁配置, 再考慮 OAuth 2.1

## Version Control
此工作區使用 Git

必須以 hexsuen 的身份提交

遠端儲存庫為: https://github.com/SuKasenWink/TheRaohe

# Philosophy of Coding

## Bug Fixing
設計時杜絕 > 執行時攔截
- Good: Make Illegal States Unrepresentable
- Bad: 在錯誤末端才打補丁, 只是掩飾病因

## Engineering
智障地必然正確 > 複雜地巧妙

## Optimization
自由度與影響半徑成反比

準則: 這優化的副作用, 會不會外洩到呼叫邊界之外?
- 不會: 怎麼寫都可以, 邏輯和語法多怪異都不是問題
- 會: 效能讓位給簡單顯而易見的契約正確性

Good:
```cpp
// 歸還時無條件重置, 契約就長在還件的那一刻, 誰改 Release 都躲不掉
void Pool::Release(NPC* N) {
    N->生命值 = 滿血;  // 無條件執行, 契約在這裡顯而易見
    FreeList.Push(N);
}
NPC* Pool::Acquire() { return FreeList.Pop(); }  // 拿到的必定乾淨, 不必猜
```
Bad:
```cpp
// 初始化邏輯放在取件端, 且用"生命值是否為 0"判斷要不要重置
void Pool::Release(NPC* N) {
    FreeList.Push(N);  // 單純歸還, 完全不管狀態, 也看不出任何"契約"存在
}

NPC* Pool::Acquire() {
    NPC* N = FreeList.Pop();
    if (N->生命值 == 0) N->生命值 = 滿血;  // 假設池子裡的都是死的
    return N;
}

// 某天新增了"撤退"機制: 未死的 NPC 也會被提前歸還池
Pool::Release(N);          // N->生命值 是 37, 未死就歸還
NPC* M = Pool::Acquire();  // 生命值 != 0, 判斷不成立, 生下來只剩下殘留生命值
```

## Single Truth
資料每次複製和流轉都會距離原始真相越遠, 因此儘可能保持單一真相

## Don't Be Attention Seeker
1. Bad: 過量 print
    - 埋沒了真正有價值的信息
2. Bad: 超長分割符 `====` & `----`
    - 干擾 註釋 / print 的資訊傳遞
    - 觀感差
3. Bad: emoji / Unicode 特殊符號

# Commenting

## What should be noted?
註釋是 What / Why, 程式碼本身是 How, 歷史交給 Git
- Good: 讀者導向: 假想成爲接手這段程式碼的人, 描述職責 / 運作預期 / 契約約束, 必要時標註重點 Bug / 關鍵決定
- Bad: 變更導向: "本次/上次改了 XXX" 只在當下有意義, 過幾次 commit 就失效, 且與 Git log 重複

Good:
```cpp
// 此處初始化緩衝區並留待 GPU 將紋理傳回 CPU
// 首次查詢時 Size 會是 (0,0), 這是正常現象——因為 GPU 回傳的數據要延遲到下個 Game Tick 才能被 CPU 讀到,
// 傳輸尚未完成一輪, 所以此處不能在第一幀就判斷失敗, 此處以 bFirst 作標記。
```
Bad:
```cpp
// 修復了上次此處的大小寫錯誤, 現在可編譯通過
```

- 當遇到一個刁鑽的 Bug(例如: 需要聚精會神, 仔細追蹤運行時, 最後發現是生命週期問題), 接手者不經意的情況下, 會再次犯錯, 那麼這個 Bug 就值得 Mark 下來


## Grammar Preference
1. 「」
2. 使用拉丁符號「, 」, 而非「，」
3. 。
4. 單行短句結尾不留「。」
