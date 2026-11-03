# pi-history

過去の Pi セッション（`<agentDir>/sessions/*/*.jsonl`）をブラウザで一覧・検索・カレンダー絞り込みする単体 exe。
Pi 拡張 `session-history.ts`（`/history`）の Rust 移植で、一覧ページと `/api/index` の JSON 形状は同一。

## 使い方

```
pi-history                 # スキャン（キャッシュ利用）→ 127.0.0.1 のローカルサーバをバックグラウンド起動 → 既定ブラウザで一覧を開く
pi-history <text>          # プロジェクト名に <text> を含むものへ事前絞り込み（大小無視）
pi-history all [<text>]    # サブエージェント一時フォルダ（フォルダ名に Temp-pi-agent-）も含める
pi-history stop            # 常駐サーバを停止
pi-history --foreground    # バックグラウンド化せずこのプロセスでサーブ（-f）
```

- 2 回目以降の起動は既存サーバを再利用します（再スキャンさせてからページを開くだけ）。
- ポート／PID／トークンは `<agentDir>/session-history/server.json`。`stop` はこのトークン付きで `/api/shutdown` を呼び、止まらなければ PID を kill します。server.json が古い（プロセスが無い）場合は自動で作り直します。
- `GET /` 一覧 / `GET /api/index` 索引 JSON / `GET /session/<id>` 個別セッション HTML（索引にある id のみ）。Host が `127.0.0.1:<port>` / `localhost:<port>` 以外は 403。
- 進捗は標準エラーに出ます。

## 設定・環境変数

| 名前 | 内容 |
|---|---|
| `<agentDir>/session-history.json` | `{ "includeSubagents": false, "port": 0 }`（毎回読む。port 0 = 空きポート） |
| `PI_CODING_AGENT_DIR` | agentDir（既定 `~/.pi/agent`、`~` 展開あり） |
| `PI_SESSION_HISTORY_NO_OPEN=1` | ブラウザを開かず URL を表示するだけ |
| `PI_SESSION_HISTORY_PI_CLI` | Pi の `cli.js` を指定（node 経由で起動） |

キャッシュ `<agentDir>/session-history/cache.json` は TS 版と JSON 互換（`{version:1, files:{<path>:{mtimeMs,size,entry}}}`）で、そのまま読み書きできます。

## Pi CLI への依存

個別セッションの HTML は `pi --export <jsonl> <out>` を子プロセス（シェル不使用）で実行して生成し、
`<agentDir>/session-history/html/<id>.html` に置きます（jsonl より新しければ再利用、120 秒でタイムアウト）。
`pi` は `PI_SESSION_HISTORY_PI_CLI` → PATH 上の `pi.exe` / `pi.cmd`（npm シムは `cli.js` を抜き出して node で直接起動）/ `pi` の順に探します。
一覧と索引は Pi / node が無くても動きます。

## ビルド・テスト

```
cargo build --release   # target/release/pi-history(.exe)
cargo test
```
