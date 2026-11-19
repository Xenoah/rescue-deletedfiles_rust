# 読取マップ付きイメージ取得

GUI・CLI・MCPから読取専用の元媒体をRAWと読取マップへ取得する。
Windows / Linux / macOSの同じRust処理を使用する。

## 操作

GUIの設定画面で元ディスクを選び「ディスクのイメージを作成・再開」を開く。
新規取得では空の出力フォルダーを指定する。取得後は保存したイメージがスキャン対象に切り替わる。
「読取マップを確認」で範囲・読取状態・試行回数・UTC時刻・OSエラーを100範囲ずつ表示する。
元ディスクからの取得先は別媒体を使う。選択したファイルを元の場所へ復元する操作は、従来どおり同一媒体を選べる。

```sh
rescue image --source /dev/sdb --destination /media/backup/acquired --progress --json
rescue image --source /dev/sdb --destination /media/backup/acquired --resume --json
rescue read-map --image /media/backup/acquired/image.rescue.raw --limit 100 --json
rescue scan --source /media/backup/acquired/image.rescue.raw --work-dir work --json
```

Windowsのディスク例は `\\.\PhysicalDrive1`。読取に必要なOS権限で実行する。
`--block-size` はセクターの倍数（最大16 MiB、既定4 MiB）。再開時も同じ値を指定する。
`--retries` は失敗範囲の追加パス数で、既定2、0〜3回。
MCPは `start_image` / `get_job` / `cancel_job` / `get_read_map` を公開する。
元ソースは明示的allowlist、出力は指定root内に制限し、同じrequest_idを再送しても別ジョブを作らない。
このMCPプロセスが生成したイメージは、そのプロセス内で `start_scan` に渡せる。

## 取得・再開の整合性

1. 初回は大きなブロックで読み、I/Oエラーの範囲を記録して先へ進む。
2. v1.2では次のパスで失敗範囲を逆方向・小ブロック（最大64 KiB）で読み、3パス目は順方向・セクター単位、4パス目は逆方向・セクター単位で再試行する。成功済み範囲は読み直さない。
3. 出力RAWを同期してから、成功範囲・SHA-256を読取マップへ追記して同期する。
4. 再開時はソースID・容量・セクター・通常ファイルの更新時刻を照合し、
   保存済みの全成功範囲を出力側のSHA-256で検証してから続行する。

通常ファイルはUnixのデバイス/inodeまたはWindowsのvolume serial/file indexを識別に含める。
ネイティブディスクではOSのデバイス識別・幾何情報を使用する。
マップは排他的な書込ロックと共有読取ロックで同時取得/解析を制御する。
出力は固定したディレクトリcapabilityから開き、保存先差替え・hardlink別名を拒否する。

ジャーナルはヘッダーのSHA-256と各76バイトレコードのCRC・連番・範囲・試行回数を検査する。
強制終了で欠けた末尾レコードは未確定として扱い、再開時に未確定末尾だけを捨てる。
途中の破損、成功済み範囲への重複書込、不正な範囲や再試行履歴は拒否する。
最初の有効なヘッダーより前に破損した取得先は、自動再初期化しない。別の空フォルダーを指定する。

## 欠損をゼロと混同しない

`image.rescue.raw` と `image.rescue.raw.rmap` を一緒に保管する。予約名 `.rescue.raw` ではマップ欠落をエラーにする。
拡張子や名前を変えてマップを取り除いた外部RAWは、取得品質を裏付ける情報がない。
未読/失敗範囲にはRAW上のゼロや古い書込途中のバイトが存在し得るが、パーサーへ有効データとして渡さない。
読取済みブロックはSHA-256を確認してから解析する。

carvingは利用可能な領域を探索し、欠損を横断する候補を対応済みにしない。
管理情報が欠ける場合は解析不良/未探索を記録する。復元時に欠損が重なるファイルは
`incomplete` とし、`missing_ranges` に元媒体の位置と、対応が分かるファイル内の位置を記録する。
圧縮ストリームでは、読めない圧縮単位全体の論理範囲を示す。
通常の先行読出し復元では、そのファイルの出力を作らない。

## 上限と検証の範囲

読取マップのジャーナルは512 MiB、区間索引は1,048,576区間、検証済みブロックのキャッシュは32 MiB。
上限では成功したことにせず停止する。既定4 MiBで4 TiB程度の健全領域を記録できるが、
細かな不良領域が多い場合は先に区間数上限に達する。
v1.2の既定モードは専用プロセスへの待機を制限する。`--read-timeout-ms 0`の直接読取モードでは、OSがI/O応答を返すまで取消が待たされる。媒体のリセット・修理は行わない。
マウント中の外部書込を停止するスナップショット機能はなく、取得前後の内容同一性を一般に保証しない。

初期の取得機能はRust 38項目・10統合群で検証した。取得の15条件には実CLIプロセスの強制終了、
未確定末尾、再開、元ソース変更、出力破損、欠損からファイル内範囲への伝播、MCP取消と出力制限を含む。
Rustの障害注入では恒久不良と一時不良、書込失敗、同時読取/書込、CRC破損を検査する。
実OSの容量不足/権限拒否・新規VHD/loop/hdiutilからの取得は、4ネイティブVMのCIを公開条件にする。
物理故障を起こした実HDD/SSDの性能を測定した結果とは区別する。

## v1.2の待機制御と互換性

`--strategy auto`（既定）は新規取得にadaptive、再開に保存済みの方式を使う。
`adaptive` / `linear` を明示できるが、再開時に方式を変更することはできない。
旧版の方式情報を持たないマップはlinearとして読み込む。新しいadaptiveマップはスキーマ2で、旧版アプリでは開けない。

`--read-timeout-ms 30000` は専用読取プロセスの1要求を待つ上限。100〜300000 ms、0なら従来の直接読み取りで待機上限なし。
`--reconnect-wait-ms 15000` はタイムアウト/切断後に同一ソースを待つ時間。0〜300000 ms、0なら自動再接続しない。
GUIは秒単位で設定する。MCPは同名のsnake_caseパラメーターを受け付ける。

タイムアウト後や取消後の結果はRAWへ保存しない。ヘルパーがOS内で停止して終了しない場合は、新しいヘルパーを増やさず部分取得で停止する。
終了できる場合でも連続3回の読取タイムアウトで停止する。読取マップは残り、元媒体を確認して再開できる。
最初のデバイス検出・出力検証やOS内I/Oの即時終了まで保証するものではない。
再接続はパスだけで判断せず、元の識別・容量・セクターサイズ・通常ファイルの更新時刻の一致が必要。

結果JSONの`passes`は**今回の実行中のみ**の方向、ブロックサイズ、試行数、コピー/失敗バイト、タイムアウト、再接続回数、所要時間を表す。
再試行したバイトは複数パスに現れるため、合計を媒体の容量と解釈しない。累積の取得範囲は`summary`と読取マップを参照する。
`stop_reason`があれば自動処理を停止した理由を示し、状態は`partial`となる。タイムアウトのマップエラー値は`-3`。

`scripts/e2e_acquisition.py`は実プロセスの停止と一時ファイルの移動で待機・取消・再接続・差し替え拒否を検証する。
物理USB/SDの故障挙動や復旧率の測定ではない。

## VHDX・AFF・E01からの取り込み

固定・可変サイズのVHDXと単一AFFは、対応するドライバーを含むThe Sleuth Kitで直接参照できます。E01ではlibewfの `ewfinfo`、`ewfverify`、`ewfexport` をPATHに置くと、取得時のエラー記録を確認してから、論理ディスクのSHA-256と書き出したRAWのSHA-256を照合します。Debian/Ubuntuでは `sleuthkit` と `ewf-tools` を使用します。

```sh
rescue filesystem --source guest.vhdx --image-type vhdx --fs-type ext4 --json
rescue import-image --source evidence.aff --image-type aff --output aff.raw --json
rescue import-image --source disk.E01 --segment disk.E02 --image-type ewf-verified --output disk.raw --json
rescue filesystem --source disk.raw --fs-type ext4 --all --destination recovered --json
```

分割E01は同じ取得セットの全セグメントを順番に指定します。既存の出力ファイルは上書きしません。取得エラーが記録されている場合、欠落セグメント、検証失敗、取消の場合は、新しいRAWを確定しません。`--image-type ewf-verified` ではlibewfによる照合が必須です。ツールの構成に応じる自動選択で照合が完了した場合、JSONに `"reader": "libewf"` と `"logical_sha256_verified": true` を記録します。

## GUIで仮想ディスク・証拠イメージを開く

1. 設定画面で「保存したイメージ」を選び、「ディスクのツール」から「イメージをRAWに変換」を開きます。
2. 元ファイルを選んで形式を確認します。分割RAW・E01の場合は、先頭のファイルに続くセグメントを順番どおりに1行ずつ入力できます。
3. 新しいRAWファイルの保存先を指定し、「変換してスキャン元に設定」を選びます。完了すると、復元元がRAWに切り替わり、バイト数とSHA-256が画面下に表示されます。

単独VDI・QCOW2とクラシックVHD（固定容量型・可変容量型）はアプリ内で読み取ります。E01・Ex01にはlibewfの `ewfinfo`・`ewfverify`・`ewfexport`、VHDX・VMDK・AFF・分割RAWには、各形式を読み取れるThe Sleuth Kitの `img_cat` が必要です。変換処理は既存ファイルを上書きせず、エラーや取消時には未完成のRAWを破棄します。

## Apple DMG（UDIF）

GUIの変換画面で「DMG (UDIF)」を選ぶか、CLIで次のように指定します。

```sh
rescue import-image --source disk.dmg --image-type dmg --output disk.raw --json
```

対象は、ディスクのセクター数を記録した単独のUDIFイメージです。XMLのブロック表から全セクターの位置を確定し、非圧縮、ゼロ領域、ADC、zlib、bzip2、LZFSE、XZ形式のLZMAをアプリ内で展開します。記録のあるデータフォーク・ブロック表・展開データのCRC-32を照合します。圧縮ブロックの入力と展開後の上限は各64 MiBです。暗号化・分割DMG・旧リソースフォーク形式や未対応コーデックでは、この方法で変換できません。

## 任意の形式を既知の配置とSHA-256から復元する

ファイルの各部分が残っている位置と削除前のSHA-256が手元にある場合、GUIまたはCLIの `recover-extents` で拡張子やファイルシステムを問わず復元できます。GUIでは設定画面で復元元を選び、「ディスクのツール」→「配置表からファイルを復元」で配置表と新しい保存先を指定します。バイト位置は入力ディスク先頭から数えます。領域は**元ファイル内の順番**に指定してください。たとえば次の配置表は、まずオフセット8192から5バイト、その後オフセット1024から6バイトを読み取ります。例のSHA-256は `hello world` の11バイトに対応します。

```json
{
  "schema_version": 1,
  "sha256": "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
  "extents": [
    {"offset": 8192, "length": 5},
    {"offset": 1024, "length": 6}
  ]
}
```

```sh
rescue recover-extents --source disk.raw --layout layout.json --output restored.bin --json
```

元ファイルでゼロと分かっているスパース領域は `{"zero_bytes": 4096}` のように指定できます。通常の読み取り領域が取得マップの欠損に重なる場合は処理を止めます。全体のSHA-256が一致したときにだけ新しい保存先を確定します。RAW、対応する読み取り専用の物理ディスク、Rescueの取得マップ・RAID構成を使用できます。配置表の上限は4 MiB・65,536領域、ファイルの上限は16 TiBです。

## 圧縮したRAWディスク

GUIの変換画面で `.img.gz` などを選ぶか、CLIで `rescue import-image --source disk.img.xz --image-type compressed-raw --output disk.raw --json` を実行します。gzip・bzip2・XZ・Zstandard・LZ4をアプリ内で展開します。同じ方式で連結された完結したストリームは順に読み、XZの4バイト単位のゼロパディングにも対応します。末尾の余分なデータ、途中切断、記録されている検査値の不一致では新しいRAWを確定しません。1イメージは最大16 TiB・65,536ストリーム、XZの復号メモリとZstandardのウィンドウは64 MiBまでです。外部辞書は扱いません。変換後に表示されるSHA-256は論理ディスク全体の値です。


## 固定容量・可変容量のクラシックVHD

GUIの「イメージをRAWに変換」でVHDを選ぶか、次のコマンドを実行します。親イメージを持つ差分VHDは対象外です。

```console
rescue import-image --source guest.vhd --image-type vhd --output guest.raw --json
```

固定容量型では末尾のフッターと論理容量を照合します。可変容量型では前後のフッター、動的ヘッダーのチェックサム、ブロック配置表、各セクターの割り当てビットを調べます。未割り当てセクターは論理ゼロとして読み、完了時にRAW全体のSHA-256を返します。検証した最大論理容量はテストイメージの68 MiBです。実装の論理容量上限は2 TiBです。


## 断片の位置が分からないファイルを復元する

削除前の原本、または同じ内容の別のコピーから、ファイル全体と各断片のハッシュを記録します。配置情報を含まないため、別のディスクに記録した後でも使用できます。

```sh
rescue fingerprint-file --source original.bin --chunk-size 512 --output original.fingerprint.json --json
rescue recover-fingerprinted --source disk.raw --manifest original.fingerprint.json --output recovered.bin --json
```

各断片のローリングハッシュでディスク上の候補を探し、SHA-256で一致した断片を元の順番で読み直します。ファイル全体のSHA-256が合ったときに新しい出力を確定します。同じ内容の断片が複数の位置にある場合、復元されたバイト列は確認できますが、削除前の物理位置を特定するものではありません。ファイルシステムや拡張子の情報は使いません。

断片サイズは512バイトから1 MiBまでの2の累乗、最大65,536断片です。各断片内のバイトがディスク上に連続して残っている必要があります。ファイルシステムの最小割当単位が不明なら、512バイトを選ぶと小さな断片に分割できます。読み取り不能範囲は検索対象から除きます。既定では全バイト位置を検索します。元の断片がセクター境界にあると分かっているときは `--alignment 512`、調べたい領域が限られているときは `--start` と `--end`（バイト単位、終端を含まない）を指定できます。


## APFSスナップショットに残ったファイル

`rescue apfs` は現在のディレクトリと保存済みスナップショットを比較し、以前のパスにだけ存在する通常ファイルを表示します。APFSに対応するThe Sleuth Kitの `pstat`・`fsstat`・`fls`・`icat` が必要です。コンテナ先頭のイメージでは次のように使います。

```sh
rescue apfs --source container.raw --json
rescue apfs --source container.raw --volume-block 32770 --snapshot-xid 3 --json
rescue apfs --source container.raw --volume-block 32770 --snapshot-xid 3 --all --destination restored --json
```

最初のコマンドでボリュームのUUID・ブロック番号と、利用可能なスナップショットのXIDを確認します。例の番号は試験用ディスクの値です。ディスク全体では `--sector-offset` でコンテナの先頭セクターを指定できます。`--all` の代わりに一覧にある `--id` を指定すると個別のファイルを復元できます。

一つのinodeに複数の過去の名前がある場合、レポートの `original_paths` にまとめ、ファイルは1件書き出します。移動・改名したファイルも、以前のパスが消えていれば候補になります。現在も同じパスにあるファイルは除外します。途中までしか読めない内容、データのないスナップショット、既存の出力ファイルには成功を報告しません。削除より前の読み取り可能なスナップショットが必要です。

## CD-ROMデータトラック

GUIの「イメージをRAWに変換」でBINファイルを選ぶか、CLIで次のように実行します。出力のISOは内容検索の復元元に指定できます。

```sh
rescue import-image --source track.bin --image-type cd-raw --output data.iso --json
rescue scan --source data.iso --mode carve --work-dir optical-scan --json
```

単一のMode 1またはMode 2 XA Form 1データトラックを読み取ります。2352バイトのセクターと、後ろに96バイトのサブチャンネルが付いた2448バイトのセクターに対応します。アドレスの連続性、EDC、P/Qパリティを各セクターで照合し、2048バイトのユーザーデータを取り出します。サブチャンネルは破棄します。CD-DA音声、XA Form 2、混在トラック、スクランブルされたセクター、損傷したセクターの訂正は対象外です。CUEを使う複数トラックでは、対象のデータトラックをあらかじめ切り出してください。

### 追記前のISOセッションから取り出す

`xorriso` が利用できる場合、標準のISO 9660またはRock Ridgeイメージのセッションを一覧にし、古いセッションにあり、現在のディレクトリでは消えている通常ファイルを復元できます。圧縮されたzisofsファイルも展開して保存します。

```sh
rescue iso-sessions --source data.iso --json
rescue iso-sessions --source data.iso --session 1 --json
rescue iso-sessions --source data.iso --session 1 --all --destination old-files --json
```

個別のファイルは `--all` の代わりに一覧の `--path '/Removed/example.bin'` を指定します。復元先では安全な名前を新しく付け、レポートの `original_path` に元の名前を残します。以前のセッションが残り、イメージのセッション表から読み取れることが条件です。名前変更や移動も候補になります。同じパスに現在も項目がある場合は候補から外れます。現在のセッションだけをコピーしたISOや、再利用され上書きされた媒体からは過去の内容を取り出せません。

## Btrfsのスナップショットと過去の世代

`rescue btrfs` は単一デバイスのBtrfsボリュームRAWからサブボリュームを一覧にします。現行サブボリュームのUUIDと、読み取り専用スナップショットに記録された親UUIDを照合してから、過去のツリーにだけ残る通常ファイルを探します。ボリューム先頭のRAWを指定します。single・DUP配置の論理アドレスを内部で解決します。DUPのメタデータは、チェックサムとブロックの識別情報を検証できたコピーを読みます。

```sh
rescue btrfs --source volume.raw --json
rescue btrfs --source volume.raw --current-root 256 --snapshot-root 257 --json
rescue btrfs --source volume.raw --current-root 256 --snapshot-root 257 --all --destination restored --json
```

番号は検証用ボリュームの例です。一覧の `id` を `--id` で指定すると個別に復元できます。元のパスと、その生バイトを16進数にした値を候補一覧に記録します。複数の名前があるinodeは一度だけ保存します。以前の名前が現在なくなったファイルが対象なので、移動・名前変更も含まれます。

CRC32C、XXH64、SHA-256、BLAKE2b-256のスーパーブロック・メタデータ・外部データを読み、FSID、世代、論理位置とキーの範囲を確認します。非圧縮・zlib・LZO・Zstandardのファイル、空ファイル、スパースファイル、共有エクステントの一部分を参照するファイル、事前割り当て領域を扱います。インラインデータはメタデータの検査で保護されます。ディスクに別途割り当てられたデータは選んだ世代のチェックサムツリーとセクター単位で照合し、圧縮データは展開後にinodeの論理サイズで切り出します。データが壊れているファイルは保存せず、バッチ内の他のファイルの結果を個別に報告します。

複数デバイスのRAIDやNODATASUMの外部データを扱う場合は、対応するLinuxカーネルで複製を読み取り専用マウントしてください。上書き済みの領域で過去のバイト列がすべて失われた場合には復元できません。

事前にスナップショットを作っていない場合にも、スーパーブロックの `backup_generations` に削除前の世代が残っていれば照合できます。番号はイメージごとに異なります。

```sh
rescue btrfs --source volume.raw --current-root 256 --previous-generation 7 --json
rescue btrfs --source volume.raw --current-root 256 --previous-generation 7 --all --destination restored --json
```

選んだ世代のチャンクツリーから当時の物理配置を読み直し、同じサブボリュームのUUIDと過去のチェックサムツリーを照合します。Btrfsは少数の過去のルート位置を循環して記録します。世代が表示されても、ツリーやデータが再利用・上書き・TRIMされている場合があります。検証できないファイルは保存しません。対応する単一デバイスのRAWでは追加ツールは不要です。[スナップショットなしで実際に削除した7ファイルの試験](../tests/btrfs-previous-roots/README.md)

GUIでは「設定」でボリュームのRAWを選び、「ディスクのツール」→「Btrfsの履歴から復元」を開きます。現行と過去の組み合わせを選び、「過去にだけ存在するファイルを表示」を押します。保存先フォルダーを指定して、選択したファイルまたはすべての候補を復元します。既存の出力は保持されます。

## AACのADTSフレームを内容から検索する

`rescue scan --source disk.raw --mode carve --extension aac --work-dir scan --json` でAAC-LCの連続フレームを探せます。対象はCRCなし、1フレーム1ブロックのモノラル・ステレオです。少なくとも3フレームで構成が一致し、各フレームの符号化データを音声に展開できた範囲を保存します。

ADTSにはファイル長や終端マーカーがありません。先頭や末尾が失われても、残った連続フレームが候補になることがあります。同じ設定の2録音が隙間なく並ぶと1件として見つかります。前置タグ、断片間の並べ替え、CRC付きADTS、HE-AAC、プログラム構成要素を必要とする音声はこの検索の対象外です。構文上正しい音声の改変を判定するには独立した原本ハッシュなどが必要です。

## TARアーカイブを内容から復元する

`rescue scan --source disk.raw --mode carve --extension tar --work-dir scan --json` は、ustar・GNU・POSIX/PAXの各メンバーヘッダーのチェックサムとサイズを検査し、連続する2つの終端ブロックまでを保存します。アーカイブの中身を復元先に展開する操作は行いません。

作成時のブロック数が分かっている場合は `--tar-blocking-factor 20` のように指定します。GNU tarの標準は20ブロックで、1ブロックは512バイトです。指定したレコード境界までゼロが実在することを確認して保存します。GUIの詳細設定「TARの保存単位」と、MCPの `tar_blocking_factor` でも同じ指定ができます。保存単位が不明な場合、原本と末尾のゼロの数が異なることがあります。

TARのチェックサムはヘッダーを保護します。メンバー内容の書き換えや、別のファイルが隣接して偶然成立したアーカイブの識別には原本ハッシュなど別の証拠が必要です。管理情報が失われたスパースファイル、分割・断片化したアーカイブ、GNU sparse拡張はこの方法では元の配置を特定できません。[実ファイルの試験条件](../tests/tar/README.md)

## ext4のジャーナルから削除前のinodeを復元する

事前に保存した配置情報がなくても、内部のJBD2ジャーナルに削除前のinodeが残っていれば、通常ファイルの配置を取り出せます。最初に候補のinodeとサイズを確認します。保存先には新しいファイル名を指定します。

```sh
rescue ext4-journal --source volume.raw --json
rescue ext4-journal --source volume.raw --inode 13 --output recovered.bin --sha256 ORIGINAL_SHA256 --json
rescue ext4-journal --source volume.raw --inode 13 --output recovered.bin --allow-unverified-content --json
```

削除後のinodeが未割り当てで、世代が一致し、データ領域が現在も空いていることを確認します。通常のext4はファイル内容にチェックサムを持ちません。削除後に空き領域が書き換わると、正しいメタデータが残っていても内容が変わります。原本のSHA-256がある場合、一致した出力だけを確定します。ハッシュがない場合は未照合の内容として明示的に保存できます。

対象はメタデータチェックサムを持つ64ビットext4、チェックサムv3の内部ジャーナル、inode内部にエクステントが収まるファイルです。ジャーナルの循環で古い世代が失われた場合、上書きやTRIMで元データが失われた場合、暗号化・インラインデータなど非対応の配置では、この方法で原本を復元できません。[実ファイルの照合記録](../tests/ext4-journal-reference/README.md)
