# Windows Media の復元試験

9本の音声・映像をFFmpeg 7.1.5の信号発生器とASFマルチプレクサで作成し、そのうち3本をGStreamer 1.26.2のasfmuxで書き直しました。ファイルごとのコーデック、バイト長、SHA-256は [manifest.json](manifest.json) にあります。WMAv1、WMAv2、WMV1、WMV2、PCM、MP3、MSMPEG4v3を含みます。ライブ配信設定で記録したGStreamerの音声を拒否試験に使います。

```sh
python tests/asf/generate.py /tmp/windows-media-originals
python scripts/e2e_asf.py --native --binary target/release/rescue
```

生成にはFFmpegとGStreamerのASFプラグインが必要です。再生成したファイルでは作成時刻や識別子が変わる場合があります。復元試験は記録済みの原本を使い、FAT32とext4に書き込み、一度読み戻してから12本ずつ削除します。通常スキャンと索引スキャン、ファイルシステム情報による復元を別々に照合します。各出力をFFmpegでデコードし、同じ環境で原本から得た非圧縮音声・映像と全バイトを比較します。Linux上のネイティブ試験にはmtools、dosfstools、e2fsprogs、Sleuth Kit、FFmpegが必要です。
