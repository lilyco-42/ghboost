# 參與指南

## 這個專案在做什麼

ghboost 是一個**給台灣使用者的傻瓜式加速工具**：按一個鈕，讓 Google、YouTube、GitHub
恢復正常存取。不想設定、不想懂協定、不想改設定檔的人，是我們的目標用戶。

核心邏輯是開源的（MIT），加速器核心用業界成熟的 [mihomo](https://github.com/MetaCubeX/mihomo)
（GPL-3.0，以獨立程式形式呼叫，不連結進我們的二進位檔）。

## 負責區域

| 區域 | 內容 | 位置 |
|------|------|------|
| hosts 演算法 | 測速、選 IP、寫入與還原 | `src/hosts.rs` |
| 節點掃描 | 找出可用節點、延遲量測 | `src/nodes.rs` |
| mihomo 行程管理 | 產生設定、啟動、監控、關閉 | `src/mihomo.rs` |
| 本地控制台 | HTTP API、零外部請求的面板、CORS | `src/web.rs` |
| 桌面托盤殼 | 狀態燈、選單、安裝/卸載腳本 | `ghboost-tray/` |
| 行動端 | Android / iOS | `ghboost-android/` |
| 落地面 | 產品頁 | `docs/index.html` |

詳細的權責說明在 [`ghboost-tray/README.md`](ghboost-tray/README.md)。

## 角色分工（Authors / Reviewers）

目前是單人維護，但**角色是分開的**，這是刻意保留的界線：

- **Author（作者）**：`lilyco-42`。提出變更、實作、開 PR。
- **Reviewer（審核者）**：負責審核與合併，**不能是同一個人**。
  目前由作者在合併前以「審核者身分」重新讀過一遍 diff 充當，
  但只要有第二位貢獻者加入，**合併權就交給對方**。

為什麼要寫死這條：我們申請的是 SignPath Foundation 的開源程式碼簽章，
它要求專案有明確的角色分工與可追溯的建置流程。角色不分開，簽章就下不來，
Windows 使用者就會一直看到藍色的「未知的發行者」警告。

## 提交前請跑

```bash
cargo fmt --check
cargo clippy --all-targets
```

Windows 托盤端在 `ghboost-tray/` 下另跑一次（它是獨立 crate，不在 workspace 裡）。

## 建置與發行

- 三個 workflow：`ci.yml`、`build-all.yml`、`tray.yml`。**全綠才能合併。**
- 發行：打 `v*` tag，workflow 自動打包並掛到 Release。
- **所有產物必須由 CI 產生**。這是拿到程式碼簽章的硬性前提——
  本機編完再上傳的不給簽。

## 兩個不能違反的約定

1. **不要自己寫協定解析器。** vless / vmess / ss / trojan / hysteria2 / tuic
   全部交給 mihomo 的 `proxy-providers` 原生處理，還自帶健康檢查。
   自己寫 = 重造輪子 + 永遠追不上新協定 + 解析錯就是連不上。
2. **不要讓使用者去 GitHub 抓東西。** 我們的目標用戶正是連 GitHub 都不順的人。
   核心與規則庫必須隨發行包附上，安裝過程不連外。

## 語言

面向使用者的文字（面板、安裝腳本輸出、落地面、錯誤訊息）用**繁體中文（台灣）**，
並套用台灣用語：影片／網路／預設／支援／身分／埠號／選單／資訊／程式／檔案／匯入／偵測。
程式註解用什麼語言都可以，目前是簡體，不強求統一。

## 授權

本專案 MIT。貢獻即表示同意以 MIT 授權公開。
