# 永続索引とスキャン再開（実装中）

この工程はまだ公開版ではない。現在はNTFSクイックスキャンのCLI/MCP経路を実装し、
GUI、FAT32/exFAT、署名探索、残存MFTへの接続と大規模評価を続けている。
v1.0の必須範囲はSPEC.mdとV1_ACCEPTANCE.mdを保持する。

候補はSQLiteのディスク索引へ追記する。最大128候補または16 MiBのバッチと、
ボリューム・MFTレコード・ストリームの再開位置を同じトランザクションで確定する。
WALとsynchronous=FULLを使い、電源断の実際の耐性はOS・ストレージの同期保証に依存する。
NTFSの親フォルダーは最大4096件のキャッシュと256階層の循環検査で解決し、全候補は保持しない。
読取り元の識別情報・容量・指紋・選択領域を再検証し、不一致では自動再開しない。
ライブ媒体のサンプル指紋は全領域不変の保証ではなく、復元前に候補の管理情報を再検証する。

ページは最大1000件/32 MiB。カーソルは索引ID、候補追加の上限位置、検索条件のダイジェスト、
並替えキーと連番を含む。取得開始後に追加された候補はそのページ列へ混入しない。
名前・パス・サイズ・作成/更新日時で並替え、別名を含めて検索できる。
日付のタイムゾーンが不明なものへUTCを付加しない。

書込み所有者はOSのファイルロックで1プロセスに制限する。読取りは同時に可能。
所有者ファイルを終了時に削除しないため、再起動時に別のinodeをロックして競合する問題を避ける。
未知の索引スキーマは変更せず拒否する。旧JSONは新しい索引へコピーし、原本を保持する。
SQLite拡張の動的ロードは使用せず、trusted_schema、トリガー、ビューを無効化する。
キャッシュは8 MiB、mmapなし、作業領域はファイル、SQL・行・バッチに上限を設ける。

```sh
rescue scan --indexed --source disk.raw --work-dir work --progress --json
rescue results --session work/ID.rescue-index/session.sqlite --sort size --limit 100 --json
rescue resume --session work/ID.rescue-index/session.sqlite --progress --json
rescue recover --session work/ID.rescue-index/session.sqlite --id mft-20-7 --archive files.zip --json
rescue index-import --session old.rescue-session.json --work-dir work --json
```

MCPはstart_scanのindexed、pause_job、resume_job、open_sessionと安定したlist_candidatesを提供する。
索引の再読込は設定済みの作業領域内に限り、元ソースの許可が必要。
復元時は選択行だけを読み、既存の先行読取り・元データ照合・保存先制約を通す。
同一媒体へ作業索引を書かない制約は、元の場所へのメモリー内復元とは別に維持する。

現時点のLinux検証は保存層4項目、実プロセス12条件。強制終了、同時所有者拒否、
取消とソース変更、MCPの停止・履歴・再送・再起動・作業領域制限、38復元ファイルを照合。
全15統合群と40コアテストを通過。大規模候補のRSS/応答・他OSでの今回の結果は未測定。
