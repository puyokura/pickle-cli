# CHANGELOG

## 0.1.0

初回リリース。Pickle の端末向けクライアントを Rust で実装し、同じバイナリが
`pickle mcp` で MCP サーバーにもなる。

### 変更点

- リポジトリ直下に Rust クレートを配置し、`Cargo.toml` / `src/` / `tests/` /
  `README.md` / `LICENSE`（AGPL-3.0-or-later）/ `.gitignore` /
  `.github/workflows/release.yml` を揃えた。
- 依存クレートから `dirs` を外した。`dirs` は `option-ext`（MPL-2.0）を引き込み、
  AGPL-3.0-or-later と互換でなかったため。状態ファイルの解決順は自前で書き出した。
- 状態ファイルの探索を `--state` → `PICKLE_CLI_STATE` → `PICKLE_STATE` →
  `PICKLE_MCP_STATE` → OS 既定の順に統一した。
- 端末の物理メモリを実測して `api/session` に載せる。以前は常に 0 を送っていた。
- 送信本文の JSON を `--json-file` / `--json-body` / `--json-body -`（標準入力）から
  渡せるようにした。`--set` だけでは配列を表現できなかった。
- macOS のメモリ計測分岐が `collect::<String>` を使っており型が不成立だったのを修正した。
  他 OS の分岐は `#[cfg]` で除外されるため Windows ホストの試験では検出できず、
  リリース CI の macOS ジョブで初めて現れた。同じ種類の取りこぼしを防ぐため、CI に
  全対象プラットフォームの `cargo check` を追加した。

### 新機能

- `pickle login` 1 コマンドでログインが完結する。番号を聞き、SMS を送り、コードを
  聞いて確定する。コードを間違えた場合は同じ `pending_id` で 3 回までやり直せる。
- 読み取り: `status` / `doctor` / `session` / `rooms` / `history`（delta・page・full）/
  `screen` / `events`（SSE の有界読み）。
- 書き込み: `send` / `react` / `read` / `mute` / `pin` / `room-create` / `leave` /
  `upload` / `upload-cancel`。
- エージェント操作: `answer` / `live-view` / `action`。
- 任意経路の `raw`。契約に無い経路は送る前に拒否し、変更系は `--confirm` を要求する。
- MCP サーバーモード（22 ツール）。`initialize` / `tools/list` / `tools/call` に加え、
  遅い呼び出しが後続を止めないよう別タスクで走らせる。
- `tests/run_all.py` で全 7 段の試験を 1 コマンドで回せる。

### 注意点

- 実アカウントでのログインが必要。`pickle login` は SMS を 1 通消費する。
- 書き込み系は実アカウントに届く。`send` は実際にメッセージを投稿し、`react` は実際に
  リアクションを付ける。宛先のルーム ID を `pickle rooms` で確認してから使う。
- `leave` は自分のキャラクター直結ルームでは 404 を返す（実測）。`GET` / `DELETE` は
  405 なので経路自体は存在するが、このルーム種別では `POST` が通らない。
- 添付アップロードはアカウントの添付枠を消費し、メッセージから参照されるまで
  ペンディングのまま残る。
- 通話（LiveKit）は実装していない。`session` の応答に `livekit_url` と `token` は
  入るが、短命の資格情報なので CLI は表示しない。
- `api/events` の `account` 以外のイベント名はアプリ側にも一覧が無い。
- クロスビルドしたバイナリの試験は実行していない。テストはホスト側ターゲットでのみ
  走らせ、クロス側はビルド成功までを確認している。
