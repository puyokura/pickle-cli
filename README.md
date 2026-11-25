# pickle-cli

Pickle を端末から操作する Rust 製ツール。
同じバイナリが `pickle mcp` で MCP サーバーにもなる。

アプリの静的解析を根拠に、アプリが実際に叩いている
REST エンドポイントだけを実装している。推測のエンドポイントは無い。

## ライセンス

AGPL-3.0-or-later。全文は `LICENSE`。

本プロジェクトは非公開プロトコルを静的解析から再構成した独立実装であり、
Pickle の配布物ではない。依存クレートはいずれも AGPLv3 と互換（MIT / Apache-2.0 /
BSD-3-Clause / ISC / Zlib / Unicode-3.0 / CDLA-Permissive-2.0）で、内訳は
`cargo tree --format "{p} {l}"` で確認できる。

## ビルド

```
cargo build --release
```

`target/release/pickle` ができる。

## まず動かす

```
pickle doctor            認証と状態ファイルと接続先の解決だけ（ネットワーク 1 回）
pickle doctor --live     ログイン後、rooms と establishment まで実測
pickle doctor --live --json
```

```
state         ok    236 bytes at %APPDATA%\pickle-cli\session.json
device        ok    present (<uuid>…)
auth          warn  not signed in
resolver      ok    https://character-api-prod.picklemem.com via pickle.com (ttl_s=300)
build         warn  client build <embedded> is older than released <advertised>
```

`state` の行は、所有者のみ読める権限なら `mode 600` も併記する（POSIX のみ）。
`doctor --live` はログイン後に `rooms` と `establishment` の 2 行を足す。

`fail` が 1 つでもあれば終了コードは 1。`warn` と `skip` は 0 のまま。

## ログイン

コマンドは 1 つだけ。番号を聞かれ、SMS が届いたらコードを聞かれる。

```
pickle login --region jp
```

`Phone number (E.164, e.g. +819012345678): ` に番号、続いて
`SMS code: ` にコードを打つ。コードを間違えると `invalid_code` を表示して
同じ `pending_id` で最大 3 回までやり直せる。成功するとセッションを保存して終わる。

MCP 側は 2 通りある。`pickle_login` は番号とコードを 1 回で渡す（呼び出しは 1 回、
サーバー往復は 2 回）。コードがまだ手元に無い場合は `pickle_login_start` で
`pending_id` を取り、SMS 到着後に `pickle_login_verify` を呼ぶ。

### 状態ファイルと既存セッション

状態ファイルは `--state`、`PICKLE_CLI_STATE`、`PICKLE_STATE`、`PICKLE_MCP_STATE` の順に
解決し、どれも無ければ `%APPDATA%\pickle-cli\session.json`（Windows）/
`$XDG_CONFIG_HOME/pickle-cli/session.json` / `~/.config/pickle-cli/session.json`。
所有者のみ読める権限で書く。

アプリが持つ `login_session.<env>.bin` は AndroidKeyStore の鍵で暗号化されていて
端末外では復号できない。CLI はそれを読まず、自前でログインし直す。

### 番号とコードの渡し方

番号もコードも argv に置かないのが既定の使い方。プロンプトで打った行は、シェルの
履歴ファイル（bash の `~/.bash_history`、PowerShell の PSReadLine 履歴）に残らない。
履歴にはコマンドの行そのものが書かれるため、引数に書けば残り、パイプ元に書いても
その行が残る。

| 渡し方 | 履歴に残るか | 使いどころ |
| --- | --- | --- |
| 引数 `pickle login --start +81...` | 残る | 使わない |
| パイプ（`--stdin` に標準入力で渡す） | パイプ元の行が残る | 使わない |
| プロンプト `pickle login --start` | 残らない | 既定 |

CLI 側は次を保証する。

| 項目 | 挙動 |
| --- | --- |
| 番号の保存 | しない。`pending_phone_code` は `pending_id` / `region` / `expires_in` の 3 キーだけ |
| 番号の表示 | しない。`login` の出力にも含めない |
| コードの保存 | しない。交換に使い、成功したら捨てる |
| 番号の送信 | する。サーバーに届く必要があるため |

### 失効した pending_id

コードが失効・期限切れすると、サーバーは 401 `code_expired` を返す。この
`pending_id` はもう交換できないので、状態ファイルから消す。

| 応答 | `pending_id` | 次の行動 |
| --- | --- | --- |
| `invalid_code`（コードが違う） | 残す | 同じ `--finish` をやり直せる |
| `code_expired`（失効） | 消す | `login --start` からやり直す |

消さないと、次の `--finish` が同じ死んだ id を再送して同じ失敗を返し、利用者は
何をすればよいか分からない。実サービスで `code_expired` を観測して確定した。

### セッションが起動中だったとき

`POST api/session` は、部屋の準備が済むまでの間 `{state, poll_after_s}` を返すことが
ある。アプリは 7 つの状態を見分ける（アプリ側の実装に合わせている）。

| 状態 | 意味 |
| --- | --- |
| `none` / `core_ready` | 準備完了 |
| `core_queued` / `core_spawning` | 起動中 |
| `room_attaching` | 接続中 |
| `room_attached` | 接続済み |
| `failed` | 失敗（`retry_after_s` 待ち） |

CLI はこの形を受け取ると `session not established yet: state=core_spawning
poll_after_s=2.5` と返す。`session_id` が無いという文言にすると、サーバーの不具合に
見えて待てばよいことが分からないため。`state` も無い応答では、これまで通り欠けた
キー名を出す。

`raw` は契約にある経路だけを通す。未知の経路は送る前に拒否し、確認されていない
メソッドは警告を出して送る。先頭に HTTP メソッドを書く形と `--method` の形は同じ。

### セッションが切れたとき

JWT の残りが 30 秒以下なら、送る前に 1 回だけ更新を試す。更新が失敗した場合の扱いは
残り時間で分ける。

| 状態 | 挙動 | 表示 |
| --- | --- | --- |
| セッション無し | 送らない | `no stored session (run \`pickle login\`)` |
| 期限切れ + 更新失敗 | 送らない | `session expired and refresh failed (run \`pickle login\` again)` |
| 期限内 + 更新失敗 | 送る | サーバーの 401 をそのまま見せる |

3 行目を残すのは、端末とサーバーの時計がずれている場合に、期限前のトークンでも通る
ことがあるため。1 行目と 2 行目を分けているのは、利用者が取る行動は同じでも、
「ログインしていない」と「切れた」は別の事実だから。

### 更新したトークンの保存

JWT を更新したときは、その値を状態ファイルに書き戻す。書き戻さないと次回起動で
また更新することになり、実行のたびに 1 往復を余計に払う。

契約試験 `a_refreshed_jwt_is_written_back_to_the_state_file` が、期限切れの JWT から
始めて「更新 → 本来の要求」の 2 リクエストが飛び、`login_session_jwt` と `expires_at`
の両方がファイル上で変わることを固定している。

## 接続先

`https://pickle.com/api/backend-endpoints` を 3 秒で読み、`environments.<env>.api_base_url`
を採る。失敗したらビルド時の既定値へ落ちる。`--env dev|alpha` で切り替える。

解決結果は `api_base` と `base_resolved_at` の 2 つで持ち、`ttl_s`（実測 300 秒）を
過ぎたら取り直す。外部から与えたベース（`--state` で渡した代替サーバーなど）は
勝手に本番へ戻さないので、フィクスチャ試験が本番へ漏れない。

## 読む

```
pickle rooms                                     ルーム一覧
pickle history <room> --mode delta --after-seq 0 --limit 50
pickle history <room> --mode page --limit 50 --before-seq 1000
pickle history <room> --mode full
pickle screen profile --query region=JP          画面データ + キャッシュキー
pickle events --frames 5 --timeout 20            api/events を 5 フレーム読む
pickle session --room-demand none               api/session
```

`history` は形態ごとに応答の型と必須キーが違う。

| モード | 応答の形 | 必須キー |
| --- | --- | --- |
| `delta` | 差分（`items` を持つ） | `v`, `items`, `reactions`, `card_states`, `next_after`, `has_more`, `head_seq`, `projection_version` |
| `page` | ページ（`messages` を持つ） | `v`, `messages`, `has_more` |
| `full` | 全量（`messages` を持つ） | `v`, `messages` |

`delta` に `next_after` が無いとポーリングが進まないので、CLI はアプリより厳しく
そこも必須にしている。`page` の配列キーは `items` ではなく `messages`。

`session --room-demand` は `none` / `voice` / `motion` / `world` の 4 値だけ。
アプリの列挙と同じで、それ以外はサーバーが 422 で弾く。
既定は `none`。

`events` は購読ではなく有界の読み取り。呼ぶたびに新しい接続になる。`account`
以外のイベントは名前と不透明なデータだけなので、CLI はそのまま返す。

### イベント購読の時間制限

REST 呼び出しのクライアントは全体 30 秒で切る。イベント購読だけは別のクライアントを
使い、読み取り 60 秒（接続 30 秒）で待つ。アプリもこの要求にだけ読み取り
タイムアウト 60 秒を設定している（アプリ側の実装に合わせている）。

理由は単純で、全体 30 秒のままだと静かなストリームが途中で切られ、`pickle events` が
「フレームが来ない」ではなく「通信が切れた」として失敗する。フィクスチャ試験
`events_tolerates_a_quiet_stream` がこの差を固定している。

### 画面名と実際の経路

`screen <name>` は名前を経路に変換する。アプリは 12 の名前を専用経路へ送り、
それ以外を `api/screens/<name>` に送る（アプリ側の実装に合わせている）。

| 名前 | 経路 | クエリ |
| --- | --- | --- |
| `profile` | `api/profile` | `region`（端末の国。`--query region=XX` で上書き可） |
| `vault` | `api/vault` | なし |
| `character` | `api/character` | なし |
| `character_settings` | `api/character/settings` | なし |
| `character_colors` | `api/character/colors` | なし |
| `character_avatar_history` | `api/character/avatar/history` | なし |
| `notification_settings` | `api/notification-settings` | なし |
| `handle_check` | `api/profile/handle/check` | `handle`（空でも送る） |
| `bubble` | `api/bubbles/<path>` | クエリは捨てる |
| `bubbles` | `api/bubbles` | `cursor` と `date` だけ残す |
| その他 | `api/screens/<name>` | そのまま |

`--query` は `key=value` を 1 引数で渡す。`screen profile` が
`api/screens/profile` を叩いていたのは実装の誤りで、直した。

### 地域とストア国

`X-Pickle-Storefront-Country` と `X-Pickle-Device-Region` は端末から決まる。CLI は
`PICKLE_STOREFRONT` / `PICKLE_DEVICE_REGION` で上書きできるが、送るかどうかの規則は
アプリのまま。

| 入力 | `Storefront-Country` | `Device-Region` | `Store-Country-Resolved` |
| --- | --- | --- | --- |
| `ja_JP.UTF-8` | `jp` | `JP` | `true` |
| `ko_KR` | `kr` | `KR` | `true` |
| `de_DE.UTF-8` | 送らない | `DE` は表に無いので送らない | `false` |
| 未設定 | 送らない | 送らない | `false` |

ストア国として認めるのは `us` / `kr` / `jp` の 3 つだけ。
それ以外は「解決できなかった」としてヘッダ自体を送らず、フラグを `false` にする
。`PICKLE_STOREFRONT=jpn` のように国コードだけを渡しても
`jp` に落ちる。

## 書く（実アカウントに届く）

```
pickle send <room> --set text="こんにちは"
pickle send <room> --set attachments=<ref> --set text=
pickle react <room> --set target_id=<id> --set target_seq=12 --set emoji=👍
pickle read <room> --seq 42
pickle mute <room> --set muted=true
pickle pin <room> --set pinned=true
pickle room-create --kind character_direct --user-id <id>
pickle upload photo.png
pickle upload-cancel <upload_id>
```

`client_msg_id` は省略すると呼び出しごとに新しい UUID を生成する。同じ値を渡すと
サーバーは再送とみなして重複排除するので、**別のメッセージを送るときは渡さない**。
再送したいときだけ同じ値を明示する。

MCP 側も同じで、1 回の `pickle_message_send` ごとに別のキーになる。契約試験
`each_send_gets_its_own_idempotency_key` がこの性質を固定している。

添付は生バイトを `POST api/attachments` に送る。multipart ではない。返る `ref`
をメッセージの `attachments` に入れた時点で確定する。使わなかった `upload_id` は
`upload-cancel` で消す。

アプリは 2 段で送り、判定は 2 つの条件の組み合わせになる（送信手順の実測に基づく）。

| 条件 | `pending=0` | pending 登録 |
| --- | --- | --- |
| `attachments` + 画像・文書 | 付けない（未確定のまま投稿で確定） | する（`ref` が UUID 形式ならしない） |
| `attachments` + 動画 | 付ける（サーバーが確定） | しない |
| `blobs` / `issue_reports` | 付ける | しない |

`pending=0` を付けた時点でサーバーは確定させているので、同じオブジェクトを pending
として登録するのは二重の指示になる。CLI は両方を導出し、`upload` の出力に
`pending_zero_sent` と `pending_registered` を出す。`--pending-zero` は互換のため
受け付けるが無視する。

### 本文 JSON の渡し方

`--set` だけでは配列（`attachments` の要素、`context.item_path` など）を表現できない。
3 つの経路を用意した。

| 経路 | 用途 |
| --- | --- |
| `--json-file PATH` | ファイルから。入れ子が深いとき |
| `--json-body JSON` | その場で書く。引用符を保つシェル向け |
| `--json-body -` | 標準入力から（UTF-8） |
| `--set key=value` | 上のどれかに対して上書き |

優先順位は file → inline → stdin の順に上書きし、最後に `--set` が勝つ。`--json` は
出力を 1 行にする既存の意味があるので、本文用には使わない。

```
pickle send <room> --json-file body.json
pickle send <room> --json-body '{"text":"hi"}'
pickle react <room> --json-file react.json
```

### ルームから抜ける

`leave <room_id>` は `POST api/rooms/{id}/leave` をボディ無しで送る。アプリも
`null` を渡しているので、ヘッダ以外は何も送らない（アプリ側の実装に合わせている）。MCP では `pickle_room_leave`。

抜けるとそのルームは自分のアカウントから消える。相手のキャラクター側の履歴も
サーバー側で扱いが変わるため、テスト用のルームで試す。

### エージェントへの回答

`answer` は `action` を必須にし、`in_call` が真偽値でなければ `false` を入れる。
`item` は `dataset` と `name` の**両方**があるときだけ送る。片方だけだとアプリは
そもそも送らないので、CLI も落とす（アプリ側の実装に合わせている）。

```
pickle answer <request_id> --set action=accept --set option_id=opt-1
pickle answer <request_id> --set action=select --set item.dataset=ds --set item.name=nm
```

`option_ids` は `--set option_ids=a,b,c` で配列になる。`value` は文字列として送る。

## エージェント

```
pickle answer <request_id> --set action=accept --set option_id=<id>
pickle live-view <request_id>
pickle action --set kind=display_name --set payload.name=...
pickle action --mode result <action_id>
```

## 生の経路

```
pickle raw api/sync                                   GET（既定）
pickle raw GET api/sync                               メソッドを先に書く形も同じ
pickle raw api/profile --method POST --set display_name=... --confirm
```

`--method` が GET 以外なら `--confirm` が要る。パスはアプリが実際に呼ぶものに
限る（`pickle contract` の一覧）。一覧に無いパスは拒否する。

## 出力

既定は整形 JSON。`--json` で 1 行。エラーは `pickle: <説明>` で stderr に出し、
終了コード 1。

## 引用符と PowerShell

Windows PowerShell 5.1 は二重引用符をネイティブプロセスへ渡す前に落とす。だから
`--json "{...}"` は成立しない。`--set key=value` は引用符を含まないのでどのシェル
でも壊れない。入れ子は `--set context.kind=card` のようにドットで書く。
真偽値・整数・小数・カンマ区切り配列は自動で型付けする。

## MCP モード

```
pickle mcp
```

stdio 上の JSON-RPC 2.0。`initialize` / `tools/list` / `tools/call` を実装する。
ツールは 22 個。

| ツール | 対応 |
| --- | --- |
| `pickle_status` | 状態と JWT 残秒 |
| `pickle_login` / `pickle_login_start` / `pickle_login_verify` / `pickle_logout` | 認証（`pickle_login` は 1 回で完結） |
| `pickle_session` | `POST api/session` |
| `pickle_rooms` | `GET api/sync` |
| `pickle_history` | delta / page / full |
| `pickle_screen` | `GET api/screens/{name}` |
| `pickle_events` | `GET api/events` 有界読み |
| `pickle_message_send` / `pickle_react` / `pickle_mark_read` | 書き込み |
| `pickle_room_settings` / `pickle_room_create` / `pickle_room_leave` | ルーム操作 |
| `pickle_attachment_upload` / `pickle_upload_cancel` | 添付 |
| `pickle_request_answer` / `pickle_action` | エージェント |
| `pickle_raw` | 任意の `api/*`（変更系は confirm 必須） |
| `pickle_doctor` | 層ごとの状態（`pickle doctor` と同じ判定） |

ツール呼び出しは読み取りループとは別タスクで走る。1 つの遅い呼び出しが後続を
止めないため。stdin が閉じても、走っている呼び出しの応答は書き終わるまで待つ。

## 検証

```
python tests/run_all.py           全 7 段を 1 コマンドで（表で結果表示）
cargo clippy --all-targets        警告 0
cargo fmt --check                 差分 0
python tests/cli_branches.py      全サブコマンドの分岐 33 項目（実プロセス）
python tests/e2e_live.py --fixture --write   全 13 段（代替サーバー）
python tests/mcp_stdio.py         MCP ハンドシェイク + 代替サーバー経由の tools/call
python tests/live_smoke.py        実サービスへの到達（認証不要の範囲）
```

3 層に分かれている。`tests/integration.rs` は実バイナリを起動して引数・出力・MCP
ハンドシェイクを見る。`tests/contract_fixture.rs` はループバックに立てた代替サーバーへ
実際に HTTP を投げ、要求ヘッダ・ボディ・応答解釈が契約どおりか確かめる（実
アカウントを使わないので SMS を消費しない）。

### 1 コマンドで全部の試験

```
python tests/run_all.py                      開発時（ネットワーク不要）
python tests/run_all.py --live               実サービスの読み取りまで
python tests/run_all.py --write --room <id>  実サービスの送信まで（宛先を明示）
```

`--write` は `--room` が無いと動かない。宛先を書かずに走らせると、たまたま先頭に
あった本物の会話へテスト文が入る。送信テストは専用の部屋 ID を渡して分離する。

段はそれぞれ別プロセス。1 段が落ちても残りは続くので、1 回の実行で全部の問題が
見える。失敗した段の名前を最後にまとめて出す。`--write` は実アカウントへ本文を
1 通送るので、使うときは宛先を確認する。

### 試験が使うバイナリ

`tests/` の 5 本はすべて `target/release/pickle` を第一候補にする。release が無い
場合、`cli_branches.py` と `live_smoke.py` は `debug/pickle` に、`mcp_stdio.py` は
`debug/pickle.exe` に落ちる（非 Windows では解決しない）。`e2e_live.py` は release
のみを見る。以前は 3 本が debug を先に見ていて、release だけを直した検証が
「通った」と誤って報告される状態だった。同じ欠陥を 1 本の制御実験で見逃したので、
探索順をそろえてある。

## 実測した範囲

この環境で実際に確認したこと。

| 項目 | 結果 |
| --- | --- |
| `cargo test` | 単体 47・契約 41・結合 8 の合計 96 件すべて成功 |
| `cargo build --release` | 成功、4.09 MB |
| MCP ハンドシェイク | `initialize` / `tools/list`（22 ツール）/ `tools/call` 3 種、応答 5 件 |
| 接続先の解決 | `pickle.com` から `ttl_s=300` を取得、base は prod |
| 無認証の `GET api/sync` | 401 `unauthenticated`（CDN の HTML 403 ではない） |
| `login --start` | `pending_id` と `expires_in=600` を取得 |
| `login --finish`（誤ったコード） | 401 `{"detail":"invalid_code"}`。`pending_id` は消費されず状態に残る |
| `doctor` の層判定 | 4 層を実測、`live` はセッション無しで skip |
| 全サブコマンドの分岐 | 33 項目（拒否系・参照系・状態・実測をすべてのコマンドで） |

未実測：認証後の `session` / `sync` / `history` / 送信。SMS のコードが要るため、
本人のログイン後に確認する。

代替サーバー（`--fixture`）で 13 段が通るのは、配線と応答解釈が正しいことの証拠で
あって、実サービスの応答が同じ形であることの証拠ではない。実サーバーの語彙
（`invalid_session`, `too_many_requests`, `room_demand` の 4 値など）は個別に実測して
実測結果として記録してある。

### 実アカウントでの実測結果

2026-11-23 に実アカウントで `session` → `rooms` → `history` 3 形態 → `screen` 3 種 →
`send` → `read` → `react` を通した。結果は次の通り。

| 段 | 結果 | 補足 |
| --- | --- | --- |
| `session` | 成功 | `session_id` と `room` が返る |
| `rooms` | 成功 | 3 室、`count=3` |
| `history`（3 形態） | 成功 | 全量は `v` と `messages` を返す |
| `screen` | 成功 | `profile` / `vault` は専用経路、`handoffs` は汎用経路 |
| `send` | 成功 | 応答の `seq` は **null**。本文は実際に届いた |
| `read` | 成功 | `head_seq` を既読位置に使う |
| `react` | 成功 | |
| `leave` | 404 | 自分のキャラ直結ルームでは通らない（`GET`/`DELETE` は 405） |

送信した本文は履歴に現れ、相手キャラの返信も届いた。`send` の `seq` が null なのは
実装の誤りではなく、サーバーがそう返すため（アプリ側の実装に合わせている）。

### 実アカウントでの疎通（最後の 1 手）

ログイン済みの状態ファイルがあれば、`tests/e2e_live.py` が session → rooms →
history を実測する。`--write` を付けたときだけ送信と既読まで進む。

```
python tests/e2e_live.py --fixture --write   代替サーバーで全段（アカウント不要）
python tests/e2e_live.py                     実サービス、読み取りのみ
python tests/e2e_live.py --write             実サービス、送信と既読まで（実測済み）
python tests/e2e_live.py --write --room <id> --text "..."   宛先と本文を指定
```

出力は段ごとに `ok` / `FAIL` を出し、失敗した段で理由を 1 行添える。トークンは
読み取りも表示もしない（状態ファイルには「セッションがあるか」だけを見る）。

`--fixture` は `tests/fixture_api.py` を loopback に立て、実測で確認した応答の形
だけを返す。session → rooms → history 3 種 →
screen 3 種 → send → read → react → leave の 13 段が通るので、実アカウントが無くても配線と
応答解釈の検証ができる。実サービスの応答が返るかは別問題で、そこは `--fixture`
を付けない実行で確かめる。

### 実測で分かったサーバー側の挙動

静的に読んだだけでは決まらない、投げて初めて分かった事実。

| 事象 | 実測結果 | 意味 |
| --- | --- | --- |
| `X-Client-Build` を欠いた `login-sessions/phone-code` | CloudFront の HTML 403 | JSON エラーではないので通信障害に見える。`X-Client-Version` だけでは通らない |
| トークン無しの `GET api/sync` | 401 `{"detail":"unauthenticated"}` | ヘッダ欠如 |
| 不正な JWT での `GET api/sync` | 401 `{"detail":"invalid_session"}` | ヘッダはあるが通らない。アプリの一覧に無い語 |
| `POST api/session` の `room_demand=launch` | 422 `literal_error`、本文が 4 値 `none/voice/motion/world` を名指し | 接続理由の列挙とは別の列挙。アプリの `room_demand` と同じ |
| 配布ビルド番号 | サーバーの `released_android_build` が埋め込み値より新しい | 版のゲートがあり得る |

`room_demand` はこれを受けて実装側で 4 値を検証し、外れた値をローカルで拒否する。
`doctor` はビルド番号のずれを `warn` として出す。

### ビルド番号のずれ

`pickle doctor` は接続先だけでなく、配布中のビルド番号も見る。サーバーが広告する
`released_android_build` がこのクライアントの埋め込み値より新しいと `warn` を出す。

```
build         warn  client build <embedded> is older than released <advertised>
```

アプリ自身が `released_android_build` を読んで古いビルドを弾く作りなので、認証後に
`session` が 4xx を返す場合、資格情報ではなく版のゲートである可能性がある。CLI は
その事実を `warn` として出し、終了コードは 0 のままにする（警告であり失敗ではない）。

### 端末メモリの申告

`api/session` の `device_memory_bytes` は、アプリが端末の総メモリを入れる値。
CLI は実測して入れる。Linux は `/proc/meminfo`、
macOS は `sysctl hw.memsize`、Windows は CIM の `Win32_ComputerSystem`
（`wmic` が無い環境向けに CIM を先に試す）。測れない環境では 0 を入れ、嘘の値を
作らない。

実測した環境では物理メモリの実値（数十 GB 規模）が送られた。値は環境ごとに変わる。

## 認可が必要な操作と、その前の確認

書き込み系は実アカウントに届く。CLI は次の 2 段で守る。

1. ローカル検証: ボディに必要なキーが無ければネットワークに出る前に拒否する
   （`send` の `text`、`react` の `target_id` と `emoji`、`read` の `--seq`、
   `answer` の `action`、`action` の `kind`）。`raw` は GET 以外に `--confirm` を要する。
2. サーバー検証: 値の妥当性はサーバーが判定する。422 の `detail` は `loc: msg` に
   平坦化して出すので、どのキーが不正かが読める。

`room_demand` だけは 1 で弾く（4 値の列挙がサーバー側で固定されているため）。

## 分かっている制約

- `X-Client-Build` は `login-sessions*` で必須。`X-Client-Version` だけでは
  CloudFront が自前の HTML 403 を返すので、通信障害に見える。
- `X-Client-Build` の値はバイナリに埋め込んだ定数。版が上がるとヘッダの値も変わる。
  解析結果を参照。
- JWT の残りが 30 秒以下になると自動で 1 回更新する（アプリと同じ余白）。
- 通話（LiveKit）は実装していない。`livekit_url` と `token` は `session` の応答に
  入るが、CLI は表示しない（短命の資格情報のため）。
- `api/events` の `account` 以外のイベント名はアプリ側にも一覧が無い。

### 既知の制約と、その根拠

| 制約 | 理由 |
| --- | --- |
| `--set` を使う。`--json '{...}'` は PowerShell 5.1 で壊れる | 二重引用符がネイティブプロセスに届く前に落ちる（実測） |
| `raw` は既知のパスしか通さない | 存在しない経路を探りに行かせない。表は実測で確認した経路から生成 |
| JWT の残り 30 秒以下で自動更新する | アプリと同じ余白。境界で 401 になるのを避ける |
| LiveKit のトークンを表示しない | 短命の資格情報。`session` の応答からは除外する |
| 状態ファイルは平文 JSON | アプリの `login_session.<env>.bin` は AndroidKeyStore の鍵で暗号化され、端末外で復号できない |

### 状態ファイルの落とし穴

`PICKLE_CLI_STATE` に空文字を渡すと、以前は「指定されたパスが見つかりません」で
止まった。空文字は経路ではないので、既定の場所に落ちるように直した。

もう 1 つ、セッションを持たないプロセスが状態ファイルを保存すると、保存済みの
セッションを消していた。消すのは `logout` だけに限定した。
