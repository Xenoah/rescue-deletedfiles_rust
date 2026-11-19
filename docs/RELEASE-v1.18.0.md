# Rescue v1.18.0

Windowsの固定・可変VHDXを内蔵の読み取り処理でRAWに変換できるようになりました。論理・物理セクターがともに4 KiBのWindowsディスクでも、NTFSの削除済みファイルを復元できます。

```sh
rescue import-image --source disk.vhdx --image-type vhdx --output guest.raw --json
rescue scan --source guest.raw --work-dir work --json
```

## 実測

| ディスク・条件 | 確認した結果 |
| --- | --- |
| Windowsで作成した固定・可変VHDX | 2台それぞれ96 MiBのゲスト全体と1 MiBごとのSHA-256がWindows側の読み取りと一致。NTFSから実際に削除したHTML・JSON・RTF・XML・PNG・ZIP・gzipの7件を各ディスクで復元し、原本の全バイトとSHA-256が一致 |
| Windowsで作成した4Kn VHDX | GPT上のNTFSから削除した7/7件を通常・索引スキャンの両方で復元。Windowsとlibvhdiが読み取ったゲストのSHA-256が一致し、Sleuth Kitのfls・icatでも削除記録と各ファイルの全バイトを確認 |
| 4 GiBの割り当て表の境界 | WindowsのNTFSパーティションを再配置し、実際の削除記録を境界前に3件、境界以降に4件置いた。QEMUで作った4,361,292,800バイトのゲストを全体のSHA-256で照合し、7/7件が原本と一致 |
| 読み取りを拒否する条件 | ヘッダー・領域表・割り当て表・親依存・ログに関する23例を拒否。冗長ヘッダーと領域表が1部ずつ正常なら全ゲストを復元。4KnではMFTレコード内の2か所の末尾をそれぞれ壊した試験と配列数の破損を検出し、残り6件は候補として維持 |
| GUI | 固定・可変・4Knの3台をRAWに変換し、元イメージの無変更、出力のSHA-256、スキャン元の切り替えを確認。4Knゲストから削除した7ファイルのGUI復元、ZIPの内容、CSV・HTMLレポートを原本と照合 |

[Windowsの検証イメージと原本ハッシュ](../tests/windows-vhdx/README.md) · [GUIの実測と画面](gui-v1.18-results.json) · [4 GiB境界の試験](../scripts/e2e_vhdx_chunk.py)

## 復元条件

単独のVHDX v1を対象に、現在のヘッダー、ブロックの実配置、論理ディスクのサイズを確認します。変換は元のディスクを変更しません。差分イメージやログの再生が必要なイメージは、対応する仮想化環境で親・ログを含むコピーから単独ディスクを作成する必要があります。ゲストデータ自体にはVHDXのチェックサムがなく、ゲスト内容の照合には別に測定したハッシュが必要です。

4Knの検証イメージでは4 KiBセクター内に1 KiBのMFTレコードがあり、各レコードの2つの512バイト単位に更新シーケンスが付いていました。復元時は両方の末尾を確認します。NTFSレコードやデータが再利用・上書きされている場合、元の内容は保証できません。標準の復元に外部ドライバーは不要です。比較試験でWindowsの仮想ディスクドライバー、QEMU、libvhdi、Sleuth Kitを使いました。[VHDXの対応条件](IMAGING.md) · [NTFSの検証](NTFS-EXTENTS.md)
