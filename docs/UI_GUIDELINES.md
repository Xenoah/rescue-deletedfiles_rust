# GUIの配置と操作の基準

調査日: 2026-11-05 JST。公式資料を読み、以下をRescueの受入基準にする。
WCAGはWeb向けの基準であり、ここではデスクトップUIの測定可能な下限として採用する。
これだけでWindowsのスクリーンリーダーやIMEの実機適合を宣言しない。

## 参照した公式資料

- [Microsoft: Responsive design techniques](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/design/layout/responsive-design.md)
- [Microsoft: Screen sizes and breakpoints](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/design/layout/screen-sizes-and-breakpoints-for-responsive-design.md)
- [W3C: WCAG 2.2 Target Size (Minimum)](https://github.com/w3c/wcag/blob/main/understanding/22/target-size-minimum.html)
- [W3C: WCAG Focus Visible](https://github.com/w3c/wcag/blob/main/understanding/20/focus-visible.html)

Microsoftは画面サイズに応じたreposition/resize/reflow/show-hide/replaceを示し、
単に画面全体を縮小する設計を避けている。W3Cは操作対象の大きさと間隔、
キーボードのフォーカスが視覚的に分かることを求めている。

## Rescueへの適用

| 規則 | 具体的な配置・挙動 | 確認方法 |
| --- | --- | --- |
| 目的と次の操作を一貫させる | 設定→スキャン→選択→復元のステップ表示。画面下の固定領域に主要操作、戻るは左、進む/復元は右 | 設定のスクロール中も主要操作が見える |
| 基本と詳細の情報量を分ける | ディスク・保存方式・保存先を先に表示。検索範囲/方式/深度/拡張子は詳細領域へ | 初期画面から保存先とスキャンへ到達できる |
| 幅に合わせて再配置する | 狭い幅では情報パネルを折畳み、フォームとツールバーを折り返す。文字の縮小で収めない | 最小ウィンドウ、通常幅、150%/200% DPIで重なりを確認 |
| 押し間違いを減らす | 操作対象は24論理px以上。主要操作は高さ36px以上、隣接操作間は8pxを基本とする | レイアウト計測と実画面 |
| 入力の意味を残す | 入力欄に常時見えるラベルを付け、例は補助情報にする | 値を入力した後も項目の意味を識別できる |
| キーボードで操作する | Tab/Shift+Tabの順序を画面の順序と一致させ、フォーカスを表示。Enter/Spaceで対象操作 | 実ウィンドウでディスク選択から復元まで操作 |
| 進行と取消を見失わない | 進捗と取消を固定領域に表示。取消待ちと停止完了を区別する | 長時間ジョブと取消の試験 |
| 一覧の比較を助ける | 名前・作成/更新日時・種類・サイズ・状態の詳細列、列ソートとグループ、選択IDの維持 | 並替え/絞込み後も選択対象が同じ |
| 不確実性をそのまま伝える | 日時・名前・削除状態・検証未実施は不明として表示し、色だけに依存しない | 不明候補と破損候補の画面 |

このファイルは実装の完了報告ではない。実装後の画面・操作結果を受入台帳とリリースへ記録する。
