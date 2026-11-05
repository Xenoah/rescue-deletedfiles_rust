# PDF・ZIP/Office・MP4/MOV carving

alpha.10で追加。GUI・CLI・MCPは同じ検証器を使う。連続配置の候補だけを扱い、
管理情報が残る場合の拡張子を問わないQuick復元とは別の対応範囲である。

| 形式 | 検証する内容 | この版の範囲 |
| --- | --- | --- |
| PDF 1.0〜1.7 / 2.0 | classic xrefの位置・世代番号、Prevの更新履歴、全間接参照、ページ階層・数・親、streamの宣言長と終端 | 暗号化・xref stream・object stream・hybrid xrefは未対応。描画・ストリーム内コーデック・JavaScriptは実行しない |
| ZIP / ZIP64 | 中央/ローカルレコードの名前・サイズ・方式、全エントリーの展開長とCRC、data descriptor、範囲の重複 | storedとDEFLATE。暗号化・分割ZIP・未知のローカルレコード間データは未対応 |
| DOCX / XLSX / PPTX | ZIPの全検証に加え、名前空間を解釈したContent Types XMLと対応する主文書エントリーで分類 | Office描画やマクロの安全性を保証しない。不正・曖昧なContent Typesは通常のZIPとして扱う |
| MP4 / MOV | ftypの対応ブランド、moov/mdat、トラック、stsd/stts/stsc/stsz/stco/co64、任意のctts/stss、全サンプルがmdat内に収まること | 非fragmented、自己完結データ参照。ftypなし、size=0、stz2、外部参照、暗号化、fragmented形式は未対応 |

PDFの終端はEOFに基づく推定で、末尾の余白までは確定できない。増分更新はxrefのPrevを検証する。
MP4/MOVは連続する対応トップレベルboxとサンプル表の範囲を確認する。
動画/音声コーデックの全フレーム復号は行わず、`media_structure`を画像の画素検証と区別する。
PDFは`pdf_structure`、ZIP系は`zip_crc_validated`を返す。

名前・階層・日時・割当・削除状態は不明。使用中ファイルや埋込ファイルも見つかる。
検証済みZIPの内部ローカルレコードは別のZIPファイルとして重複列挙せず、
中に保存された独立したZIPはその独立した終端/CRCが検証できれば別候補として残す。
復元時は形式を候補IDから確認し、ソースを再解析して全ファイルのSHA-256を照合する。
保存済みJSONのサイズ・名前から任意の読取範囲や出力パスを作らない。

## 資源上限

- 符号化された候補は64 MiB。共通の検証予算は1スキャン4 GiBで、読取・展開・検証作業に課金する。
- PDF: 65,536オブジェクト、128更新、深さ64、全オブジェクト合計1,048,576トークン。
- ZIP: 4,096エントリー、1エントリー64 MiB、展開合計128 MiB。Office分類XMLは1 MiBまで。
- MP4/MOV: 256トラック、テーブル1,048,576エントリー/サンプル、トップレベル65,536 box。
- 暗号・巨大サイズ・未対応形式を通常ファイルとして成功扱いしない。取消を読取と検証の途中で確認する。

大容量の連続動画などが上限を超える場合は、管理情報による復元が可能かQuickで確認する。
構造やCRCの整合性は削除前の内容との同一性を証明しない。正解データとの一致は試験時だけ測定する。

## 検証

`scripts/e2e_documents.py` は独立したReportLab/PyMuPDF、python-docx、openpyxl、
python-pptx、FFmpegで生成・再読込した実ファイルと、別実装の合成境界条件を使用する。
69ケース: 正常23変種、破損/未対応30条件、読取窓境界、範囲、ZIP内ZIP、実stdio MCP。
CLI/MCPから30出力を正解バイトと照合する。coreでは1,024種類の決定的な改変・切断入力も検査する。

Linux実GUIでも7形式を探索・選択・ZIP保存し、全バイト・CRC・SHA-256・ソース不変を照合した。
[GUI記録](gui-document-results.json) とリリースの `document-results-*.json` を参照。
3 OSのネイティブVMには同じ試験をdebug/release両方で登録し、公開ポータブルの実CLIでも再実行する。

