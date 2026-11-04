# インストールとポータブル起動

配布元は[GitHub Releases](https://github.com/Xenoah/rescue-deletedfiles_rust/releases)。OSとCPUに合う版を選ぶ。
各配布物のSHA-256は同じリリースのSHA256SUMS.txtで確認できる。
入力として解析できるファイルシステムは各OS共通でNTFS・FAT32・exFAT。Linuxのext4やmacOSのAPFSを解析する機能はまだない。

## Windows x64

セットアップEXEを実行すると、既定ではユーザーのLocalAppData/Programs/Rescueにインストールする。
インストールに管理者権限は不要。ディスクを直接スキャンする際は管理者として起動する。スタートメニューからRescueを起動し、Windowsの「インストールされているアプリ」から削除できる。
CLIはインストール先のrescue.exeを使う。PATHは自動変更しない。

ポータブルZIPは任意のフォルダーへ展開し、rescue-gui.exeを起動する。
alpha.6以降の配布ビルドはVCランタイムを静的リンクする。GUIはDirect3D 12（wgpu）を使用し、GPUのない環境ではWindowsのWARPソフトウェア描画を利用できる。Windows 10/11の個別バージョンと実機操作は別途検証が必要。
発行者署名はなく、SmartScreenが初回起動の確認を表示する場合がある。

## Linux x64

glibc 2.35以降を必要とする。インストール版はDebian/Ubuntu系のDEB。

```sh
sudo apt install ./rescue-0.2.0-alpha.7-linux-x64.deb
rescue-gui
rescue --version
sudo apt remove rescue-deletedfiles
```

実体は/opt/rescue、コマンドは/usr/bin/rescueと/usr/bin/rescue-gui、デスクトップメニューも登録する。
削除操作はインストールしたプログラムを対象とし、ユーザーが復元したファイルを削除しない。

ポータブル版はtar.gzを展開して、その中のrescue-guiを実行する。CLIは同じフォルダーのrescue。
X11またはWayland、OpenGL、libxkbcommon等の実行時ライブラリが必要。DEBのDependsに記録している。
ファイル選択ダイアログにはxdg-desktop-portalとデスクトップ環境用のバックエンド、日本語表示にはfonts-noto-cjkを推奨する。
パスをテキスト欄へ入力することもできる。

## macOS 13以降

Apple Silicon用arm64か、Intel用x64を選ぶ。
PKGは/Applications/Rescue.appを配置し、/usr/local/bin/rescueをアプリ内CLIへのシンボリックリンクとして登録する。
ApplicationsからRescueを起動する。CLIは`rescue --version`で確認できる。

ポータブルZIPは展開してRescue.appを任意の場所から起動する。
CLIは`Rescue.app/Contents/MacOS/rescue`。PKGを使わない場合、PATHやシステムのコマンドは変更しない。

アプリはアドホック署名のみ。Apple Developer ID署名・公証はしていない。
ダウンロード後にGatekeeperで止まる場合、macOSの「システム設定 → プライバシーとセキュリティ」で対象を確認して起動を許可する。

削除する場合はRescue.appと、PKGが登録した/usr/local/bin/rescueのリンクを削除する。
macOSの実機による初回インストール・手動GUI操作と、CIのPKG展開検証は区別している。

## 共通

解析するディスク・ボリューム・RAWイメージは読み取り専用で開く。復元先は元媒体と別のハードリンク対応媒体にする。
ZIP保存は作業フォルダーへ一度復元してから無圧縮ZIP64を作るため、ファイル合計とZIP分の空き容量が必要。
作業フォルダーを空欄にしたGUIでは保存先の.rescue-workを使う。セッションを再利用する間は保持する。
取消・失敗時の一時保存先は結果に表示し、成功したZIPだけを最終ファイル名で公開する。

## ディスクを直接スキャンする場合

Windowsは管理者として起動します。LinuxとmacOSは対象デバイスを読む権限で起動してください。アプリ自身は読み取り専用ハンドルを使います。元ディスクのアンマウント・オフライン化は利用者がOSで行います。作業フォルダーと復元先は別の物理ディスクへ設定します。同じ媒体の別パーティション・別ドライブ文字も保存先にできません。媒体を識別できないネットワーク出力なども安全側で拒否します。ディスクイメージを入力する補助モードではこの実デバイス間の制約は適用しません。
