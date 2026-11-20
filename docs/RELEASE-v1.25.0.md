# Rescue v1.25.0

GNU ddrescueのRAWとmapfileを一緒に取り込めるようになりました。Rescueは取得済みの範囲だけを復元に使用します。たとえば、読み取りに失敗したセクターに以前のWAVのデータが残っていても、そのデータを復元結果に含めません。読取マップで区別した失敗・未試行の範囲は、その後のスキャンでも確認できます。

```sh
rescue import-ddrescue --source capture.dd --map capture.map --destination mapped --json
rescue read-map --image mapped/image.rescue.raw --json
rescue scan --source mapped/image.rescue.raw --work-dir scan --json
```

GUIの「イメージをRAWに変換」で「GNU ddrescue (RAW + map)」を選び、取得RAW・mapfile・新しい保存先フォルダーを指定します。変換後のRAWをスキャン元に設定し、取得・失敗・未試行のバイト数を表示します。CLIでは入力と出力の位置が異なる部分取得や、末尾の欠損でRAWが短い取得にも対応します。[操作と対応範囲](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.25.0/docs/IMAGING.md)

## 実ファイルでの検証

| 取得条件 | 復元結果 |
| --- | --- |
| FAT32とext4をGNU ddrescueで全域取得 | BMP・GIF・PNG・PDF・WebP・WAV・ZIPの実削除を各7件復元。各ファイルの全バイトとSHA-256が原本に一致 |
| Windowsで取得した96 MiBのNTFSを全域取得 | Windowsが記録したディスクSHA-256と一致。HTML・JSON・RTF・XML・PNG・ZIP・gzipの実削除7件も元の名前と内容で一致 |
| FAT32とext4で各1セクターの読み取り失敗と1セクターの未試行を模擬 | GNU ddrescuelogが示したセクター番号と状態に一致。失敗セクターに元のWAVの正しいバイトが残った状態でもWAVは出力されず、他の6ファイルは両方で原本と一致 |

GNU ddrescue 1.23と1.29で上記をそれぞれ再現しました。Linux実GUIでも不正なmapfileを拒否し、正しいマップを選ぶと取得41,942,016バイト、失敗512バイト、未試行512バイトと表示しました。GUIが設定したスキャン元からCLIで6件を復元しました。完全なPDFの直後に別のデータがあり、その先に欠損がある場合にも、PDFの末尾を認識できるようにしました。[GNU実証のSHA-256](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.25.0/docs/ddrescue-v1.25-results.json)・[GUI検証](gui-v1.25-results.json)

ローカルのRustテスト96件、統合テスト82組、35形式・36原本の比較試験を通過しました。比較試験1,116件中1,016件が原本と完全一致し、欠損・上書きなどを含む残りは各条件の期待値どおりでした。

## 復元できる範囲

出力RAWのSHA-256は、未取得領域をゼロにした保存結果の値です。GNUマップには元データの内容ハッシュがないため、同じ取得で作ったRAWとマップを使用してください。内部境界が512バイト単位でないマップ、範囲の矛盾、取得済み範囲の途中で切れたRAWは拒否します。上書きやTRIMで消えたデータには別のコピーが必要です。

既存の取得ファイルの取り込みには外部ツールは不要です。この実証ではGNU ddrescueとddrescuelogで取得と状態照合を行い、mtools・e2fsprogs・QEMUでテストディスクを作成または書き出しました。読み取り失敗はGNUのテストモードで制御しました。
