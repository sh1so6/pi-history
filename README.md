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
pi-history --agent-dir <path>  # agent フォルダ（`sessions` を含む）を直接指定
pi-history --pick-dir      # フォルダ選択ダイアログで選び直す（保存済みでも上書き）
pi-history --no-dialog     # ダイアログを一切出さない（エラーは標準エラーのみ）
```

### 単体起動（ダブルクリック）とフォルダ選択

引数も `PI_CODING_AGENT_DIR` も無しで exe を起動しても使えます。agentDir は次の順で決まります。

1. `--agent-dir <path>`（`--agent-dir=<path>` も可）
2. 環境変数 `PI_CODING_AGENT_DIR`
3. アプリ設定 `app.json` の `agentDir`
4. 既定 `~/.pi/agent`

決まった agentDir に `sessions` フォルダが無い場合（または `--pick-dir` 指定時）は、OS ネイティブのフォルダ選択ダイアログ
（Windows は Explorer 形式）が開きます。「Pi の agent フォルダ（`sessions` を含む）」か「`sessions` フォルダ自体」を選んでください
（`sessions` 自体を選んだ場合は親が agentDir）。`sessions` を含まないフォルダを選ぶとエラーを出して再選択になり、キャンセルすると終了します。
選んだ agentDir は `app.json` に保存され、以降は聞きません。初期フォルダは `~/.pi`（あれば）。

- `app.json` の場所: Windows `%APPDATA%\pi-history\app.json` / macOS `~/Library/Application Support/pi-history/app.json` / その他 `$XDG_CONFIG_HOME`（なければ `~/.config`）`/pi-history/app.json`。形式は `{ "agentDir": "C:\\Users\\you\\.pi\\agent" }`。壊れていれば無視します。`PI_HISTORY_CONFIG_DIR` でフォルダを差し替え可能（テスト用）。
- ダイアログは常駐化の前に、ユーザーが起動した側のプロセスで出します。選んだ agentDir は常駐側へ `--agent-dir` で渡します。`stop` はダイアログを出しません（`sessions` が無ければ「server is not running」）。
- sessions が見つからない・選択キャンセルなどの致命エラーは、標準エラーに加えてメッセージボックスでも表示します（コンソールが即閉じても分かるように）。
- `--no-dialog`、`PI_SESSION_HISTORY_NO_DIALOG=1`、`PI_SESSION_HISTORY_NO_OPEN=1` のいずれかがあるとダイアログ／メッセージボックスを一切出さず、標準エラーのみで終了コード 1 になります（自動テスト・CI 向け）。

- 2 回目以降の起動は既存サーバを再利用します（再スキャンさせてからページを開くだけ）。
- ポート／PID／トークンは `<agentDir>/session-history/server.json`。`stop` はこのトークン付きで `/api/shutdown` を呼び、止まらなければ PID を kill します。server.json が古い（プロセスが無い）場合は自動で作り直します。
- `GET /` 一覧 / `GET /api/index` 索引 JSON / `GET /session/<id>` 個別セッション HTML（索引にある id のみ）。Host が `127.0.0.1:<port>` / `localhost:<port>` 以外は 403。
- 進捗は標準エラーに出ます。

## 設定・環境変数

| 名前 | 内容 |
|---|---|
| `<agentDir>/session-history.json` | `{ "includeSubagents": false, "port": 0 }`（毎回読む。port 0 = 空きポート） |
| `PI_CODING_AGENT_DIR` | agentDir（`--agent-dir` の次に優先。既定 `~/.pi/agent`、`~` 展開あり） |
| `PI_SESSION_HISTORY_NO_DIALOG=1` | ダイアログ／メッセージボックスを出さない（`--no-dialog` と同じ） |
| `PI_HISTORY_CONFIG_DIR` | `app.json` を置くフォルダの上書き |
| `PI_SESSION_HISTORY_NO_OPEN=1` | ブラウザを開かず URL を表示するだけ（ダイアログも出さない） |
| `PI_SESSION_HISTORY_PI_CLI` | Pi の `cli.js` を指定（node 経由で起動） |
| `PI_SESSION_HISTORY_NO_INJECT=1` | `/session/<id>` に見やすさ改善の CSS/JS を注入せず、素の `pi --export` HTML を返す |

キャッシュ `<agentDir>/session-history/cache.json` は `{version:4, files:{<path>:{mtimeMs,size,entry,parentSession?,daily?,cacheDaily?,cacheEvents?}}}`。`entry` は従来どおり（TS 版と同形）ですが、`version` が違う旧キャッシュは捨てて全再パースします（TS 版とはもう共有しません）。

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

## キャッシュ統計（`cache` / `cacheEvents`）

プロンプトキャッシュの**ミス**と、relay / compaction による**コンテキスト縮小と再キャッシュ**を jsonl から算出し、`/api/index` のトップレベル `cache` と `cacheEvents` に出します（`entries` / `spend` の形状は不変）。

```
cache:       [{date, project, sub, misses, missTokens, missCost, missTtl, missTtlTokens, missModel, missModelTokens,
               relays, relayBefore, relayAfter, relayDropped, relayRecache, relayRecacheCost,
               relayPostRequests, relaySavings,
               compacts, compactBefore, compactAfter, compactDropped, compactRecache, compactRecacheCost,
               compactPostRequests, compactSavings}]
cacheEvents: [{ts, date, project, sub, kind:"relay"|"compact", before, after, dropped, recache, recacheCost,
               postRequests, savings, net, breakEven, open, sid, trigger}]
```

- 件数・トークンは整数、`*Cost` は USD（f64）。原因「その他」は `misses - missTtl - missModel` でフロントが出す。全部 0 の行は出さない。
- **集計**は `spend` と同じ: 日付 = メッセージ／compaction の `timestamp` のローカル日付、集計単位 (date, project, sub)、サブエージェントは `parentSession` を辿って親の project に合算し `sub:true`（辿る処理は `owner_projects()` を `build_spend` と共有）。`cacheEvents` は `ts`（ISO 文字列）昇順で、`sid` はセッションヘッダの id（一覧の id の重複解消リネーム前の値）。
- **ツリーで辿る**: 各行の `id` / `parentId` から親子を作り、直前の応答は `parentId` を遡った最寄りの「有効な assistant 応答」（ファイル順ではない。`/tree` の分岐は枝ごとに別の祖先と比べる）。親が見つからなければ直前なし＝ミスにしない。
- **有効な応答**: `usage` あり、`stopReason` が `error` / `aborted` 以外、`input + cacheRead + cacheWrite > 0`。それ以外の行（user / toolResult / custom …）や無効な応答は素通しで、さらに親を遡る。**compaction で遡りは止まる**（compaction の直後の応答は「直前なし」）。
- **ミス**: 応答 m の直前 p について `expected = p.cacheRead + p.cacheWrite`。`expected >= 4000`（`MISS_MIN_EXPECTED`）かつ `m.cacheRead < expected * 0.5`（`MISS_RATIO`。ちょうど半分はヒット）ならミス。`missTokens = expected - m.cacheRead`。
  - 原因: provider+model が p と違えば `model`。同じで `api == "anthropic-messages"` かつ「m.開始 − p.開始（`message.timestamp`）」が TTL を超えたら `ttl`（TTL は、系列上で最後にキャッシュ書込（`cacheWrite>0`）した応答が `cacheWrite1h>0` なら 3,600,000ms、それ以外 300,000ms。ちょうど TTL は超えていない扱い）。それ以外は「その他」。Anthropic 以外は `ttl` にならない。
  - `missCost` = 再キャッシュ分 `min(missTokens, m.cacheWrite)` × (`m.cost.cacheWrite / m.cacheWrite`)。書込みの請求のうち再書込みした部分の推定。`m.cacheWrite == 0` か書込み料金が 0 なら 0。
- **relay / compaction イベント**: `type:"compaction"` の行ごとに 1 件。`details.relay === true` なら `kind:"relay"`（context-relay プラグイン）、それ以外は `"compact"`（Pi 標準の compaction）。`before = tokensBefore`（推定値）。compaction の次の最初の有効な応答 m で確定: `after = input + cacheRead + cacheWrite`、`recache = m.cacheWrite`、`recacheCost = m.cost.cacheWrite`、`dropped = max(0, before - after)`。この m はミスに数えない。後続の応答が無ければ `after = recache = dropped = 0`（不明）のまま出す。`relayDropped` などは `cache` 行では種別ごとの合計。
- **損益分岐（`postRequests` / `savings` / `net` / `breakEven` / `open`）**: 「relay / compaction をしなかったら、以降の全リクエストが `dropped` トークン分余計にキャッシュ読みしていた」という反実仮想で、再キャッシュ代が回収できたかを見る（定数・コメントは `cachestat.rs` 冒頭）。
  - **epoch**: 各 compaction は自分の epoch を持つ。ツリーの各行は親から `epoch`（イベント番号）を継承し、compaction の行は自分のイベントを epoch にする。つまり epoch は「同じ祖先チェーン上の次の compaction まで」で、枝は自分の祖先に従う（別枝のリクエストは自分の祖先の epoch に数える）。
  - epoch 内の有効な assistant 応答ごと（compaction 直後の最初の 1 件を含む）に `postRequests += 1`、`postCacheRead += m.cacheRead`、`postCacheReadCost += m.cost.cacheRead`（後ろ 2 つは cache.json にのみ保存）。
  - `rate = postCacheReadCost / postCacheRead`（`postCacheRead == 0` なら不明）。`savings = postRequests × dropped × rate`（USD。rate 不明なら 0）。`net = savings − recacheCost`（正なら回収済み）。`breakEven = recacheCost / (dropped × rate)`（リクエスト数。`dropped == 0`・rate 不明・rate 0（`cost.cacheRead` 無し）なら `null`）。
  - `open`: そのセッションファイルの**最後の** compaction イベントなら `true`（進行中、またはセッションがそこで終わった。数値はまだ増えうる）。それより前のイベントは `false`。
  - `cache` の日別行の `relayPostRequests` / `relaySavings` / `compactPostRequests` / `compactSavings` は、既存の `relay*` / `compact*` と同じ (date, project, sub) で合計（全部 0 の行を出さない規則は不変）。`savings` / `net` / `breakEven` / `open` は集計時に毎回計算する。
- **`trigger`**（`"manual" | "input" | "expiry" | null`）: `<agentDir>/context-relay/log.jsonl`（`{at, sessionId, trigger, ok, …}`）と突き合わせる。relay イベントのみ、`sessionId == sid` かつ `ok == true` かつ時刻差が 120 秒以内（`TRIGGER_MATCH_MS`）の最も近い行を 1 行 1 回だけ使う。ログが無い・読めない・一致なし・compact は `null`。キャッシュファイルには保存せず、集計時に毎回付ける。
- **実装**: 行は 1 回の走査で処理。2000 バイト超の行は従来どおり JSON 全体は読まず、`type:"compaction"` は必要な数項目だけ（`summary` / `systemMessage` は読み飛ばし）、他の巨大行は先頭（`"id"`/`"parentId"` が `type` の直後）か末尾（custom 系は末尾に `id`/`parentId`/`timestamp`）から `id` / `parentId` だけ取り出して系列を保ちます。セッションごとの結果（`cacheDaily` / `cacheEvents`）は cache.json に保存（`CACHE_VERSION` 4。旧キャッシュは自動で捨てて再パース）。日付を確定して保存するため、タイムゾーンを変えたら cache.json を消してください。
- 実データ（約 1800 ファイル / 9.8GB）での目安: cache.json 約 9.3MB、`/api/index` 約 7.3MB（`cache` + `cacheEvents` は約 90KB）、全再スキャン約 0.9 秒・キャッシュ利用時 約 0.1〜0.2 秒（16 スレッド、OS ファイルキャッシュ温存時）。

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
