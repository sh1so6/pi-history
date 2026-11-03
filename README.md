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
| `PI_SESSION_HISTORY_NO_INJECT=1` | `/session/<id>` に見やすさ改善の CSS/JS を注入せず、素の `pi --export` HTML を返す |

キャッシュ `<agentDir>/session-history/cache.json` は `{version:2, files:{<path>:{mtimeMs,size,entry,parentSession?,daily?}}}`。`entry` は従来どおり（TS 版と同形）ですが、`version` が違う旧キャッシュは捨てて全再パースします（TS 版とはもう共有しません）。

## 支出（`spend`）

一覧の上の「期間の支出」パネルは `/api/index` のトップレベル `spend` 配列から作ります（既存の `entries` などの形状は不変）。

```
spend: [{date, project, model, input, output, cacheRead, cacheWrite, cost, messages, sub}]
```

- **日付基準**: jsonl の assistant メッセージ（と `type:"usage"` 行）の `timestamp` を**このマシンのローカル日付**（`YYYY-MM-DD`）に直したもの。セッション開始日ではなく、日をまたぐセッションはメッセージごとに日別に割れます。キャッシュには日付を確定して保存するため、タイムゾーンを変えた場合は cache.json を消すと再計算されます。
- 集計単位は (date, project, model, sub)。`cost` は `usage.cost.total`、トークンは `usage.input/output/cacheRead/cacheWrite`。`messages` は assistant 応答数（`usage` 行は 0）。timestamp が無い行は直前の行の日付、model が無ければ `unknown`。
- `project` は一覧と同じ規則（cwd 末尾 2 セグメント、なければフォルダ名）。全期間の `cost` 合計は各セッションの `cost` の総和（サブエージェント含む）と一致します。
- **サブエージェント**（フォルダ名に `Temp-pi-agent-`）の支出は、先頭ヘッダの `parentSession` を再帰的にたどり、サブエージェントでない最初のセッションの project に合算し `sub:true` を付けます（最大 10 ホップ・循環検出あり。親が見つからない／辿れない場合は元の project 名のまま別扱い）。通常セッションの `parentSession`（relay / fork）は独立した project のままで親には寄せません。
- `includeSubagents`（既定 false）でも `spend` にはサブエージェント分を必ず含めます（一覧の行が隠れるだけ。スキャンは常に全フォルダを見ます）。
- パネルはカレンダーで選んだ日／範囲（未選択は全期間）とプロジェクト絞り込みに従い、**検索語は適用しません**。

## 個別セッション画面の見やすさ改善（配信時注入）

`GET /session/<id>` は、キャッシュ済みの export HTML を**書き換えず**、レスポンス生成時に
`src/inject.css` を最初の `</head>` の直前、`src/inject.js` を最後の `</body>` の直前へ挿入して返します
（`src/inject.rs` の `inject()`。`</head>` が無ければ先頭、`</body>` が無ければ末尾。マーカー `<!--pi-history-inject-->` で二重注入防止）。
CSS/JS を直しても export の再生成は不要で、再ビルド（`include_str!`）だけです。外部リソースは使いません。
問題があれば `PI_SESSION_HISTORY_NO_INJECT=1` で素の export を表示できます。

- 読みやすさ: 本文幅 800→980px、本文は日本語に向くサンセリフ（コード・ツール出力は等幅）、行間拡大、コードブロック折返し、
  user/tool のカード化（テーマの背景が単色でも区別できる）、展開したツール出力は最大 `70vh` でスクロール。
- thinking は既定で折りたたみ（ブロックをクリックで個別に展開。`T` や上部バーの `thinking` ボタンで全体切替）。ツール出力は export 本来の既定（先頭数行＋展開）のまま。
- 上部に固定の要約バー: `← 一覧に戻る`（`/`）、プロジェクト、期間、メッセージ数（user / assistant / tool calls）、コスト、トークン（↑in ↓out R=cacheRead W=cacheWrite）、モデル、thinking/tools 切替。
- 各 assistant メッセージの時刻の横に小バッジ（そのメッセージの cost とトークン）。
- 依存する export の構造: `#session-data`（base64 の JSON。`entries[].message.usage/model`, `header`）、`#content` / `#messages`、
  `.assistant-message#entry-<id>`、`.thinking-block/.thinking-text/.thinking-collapsed`、`.tool-execution`、`.markdown-content`、
  ヘッダーの `[data-action=toggle-thinking|toggle-tools]`。
- フェイルセーフ: 機能ごとに try/catch。セレクタやデータ形状が想定と違えば何もしない（元の表示のまま）。
  配色は export の CSS 変数（`--text` `--dim` `--accent` `--body-bg` …）に追従するため、ダーク/ライトも export のテーマのまま。
- 見た目の比較画像: `docs/session-before.png`（素の export）/ `docs/session-after.png`（注入後）/ `docs/session-after-mobile.png`（幅 480px）/ `docs/session-font.png`（`Aa` ポップオーバー）。

### 本文フォントの変更（上部バーの「Aa」）

要約バーの `Aa` ボタンでポップオーバーが開き、右側の本文領域（`#content`）の見た目を変えられます（左のツリーは対象外）。

- 本文: フォント（プリセット select ＋ 自由入力 `font-family`。**入力欄があればそちらが優先**）、サイズ 12〜24px、行間 1.3〜2.2。
- コード・ツール出力: 等幅フォント（select ＋ 自由入力）、サイズ 10〜24px。`リセット` で既定に戻します。
- 適用は CSS 変数 `--ph-font` `--ph-font-size` `--ph-line` `--ph-mono` `--ph-mono-size`（`inject.css` が参照。未設定なら従来の見た目）。
  変更は即時反映され、350ms 後に保存します。
- **保存先**: `<agentDir>/session-history/ui.json`（サーバ側。ポートが毎回変わっても全セッション共通で残ります）。
  併せて `localStorage`（キー `pi-history.sessionFont.v1`）にも書き、サーバに保存できない場合のフォールバックにします
  （サーバに `ui.json` がまだ無く localStorage にだけある場合に限り、そのコピーを適用し、次回ページを開いたときサーバへ引き継ぎます。`リセット` は空の `ui.json`（`{}`）を残すので、他オリジンの古い localStorage が復活することはありません）。
- ちらつき防止: サーバが `ui.json` の内容を `<style id="pi-prefs">:root{--ph-…}</style>` として `</head>` 直前へ埋め込んで返します（設定が無ければ何も足しません）。
- API: `GET /api/ui-prefs` → `{ok, prefs|null}`。`POST /api/ui-prefs`（本文は JSON オブジェクト、`{"reset":true}` で削除）は
  Host 検証に加え、`Content-Type: application/json` 必須・`Origin` が自サーバ（`http://127.0.0.1:<port>` / `http://localhost:<port>`）と一致する場合のみ受理（`server.json` のトークンは不要）。本文は最大 8KB。
- サニタイズ（`src/prefs.rs`）: font-family は 200 文字以内、英数字（日本語可）・空白・`, - _ .`・対になった引用符のみ許可し、`; { } < > ( ) \ / : @ !` などを含む値は捨てます。数値は範囲外なら捨て、刻みに丸めます。
- 既知の制限: 同じ `ui.json` を複数ブラウザで共有（最後に保存した設定が全部に効く）。開いたままの他タブには再読み込みするまで反映されません。

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
