<p align="center">
  <img
    src="assets/moli-browser-banner.jpg"
    alt="Moli Browser — 構造を優先し、ピクセルは必要なときだけ。AIエージェント向けのオープンソースブラウザ。"
    width="1086"
  />
</p>

<h1 align="center">Moli</h1>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.zh-CN.md">简体中文</a> |
  <strong>日本語</strong> |
  <a href="README.de.md">Deutsch</a> |
  <a href="README.fr.md">Français</a> |
  <a href="README.es.md">Español</a>
</p>

Moli は、AI エージェント向けに設計された本番投入可能なヘッドレスブラウザです。レイアウトとレンダリングをオンデマンドで行うことで、フル機能のブラウザランタイムと軽量なリソース消費を両立しています。

AI エージェントによる Web ページの取得・抽出、Web 検索、ブラウザ操作の自動化に対応します。

CLI、CDP、WebDriver Classic、WebDriver BiDi のいずれからでも利用できます。

## クイックスタート

以下の文章を AI コーディングエージェントに渡してください。

```text
https://github.com/lexmount/moli/tree/main/skills 以下の skills をインストールし、
その指示に従って最新のビルド済み Moli バイナリをダウンロード・インストールしたうえで、
moli-webfetch を使って https://example.com を取得し、結果を見せてください。
```

## デモ

<p align="center">
  <a href="assets/moli-game.jpg">
    <img
      src="assets/moli-game.jpg"
      alt="Moli でレンダリングし、Chrome DevTools で検証している HTML5 ゲーム"
      width="1200"
    />
  </a>
</p>

<p align="center">
  <sub>Moli でレンダリングし、Chrome DevTools からリアルタイムに検証している HTML5 ゲーム。</sub>
</p>

<p align="center">
  <a href="assets/moli-devtools-rust-lang.jpg">
    <img
      src="assets/moli-devtools-rust-lang.jpg"
      alt="Moli でレンダリングし、Chrome DevTools で検証している rust-lang.org"
      width="1200"
    />
  </a>
</p>

<p align="center">
  <sub>Moli でレンダリングした rust-lang.org。ライブの DOM、CSS、ジオメトリを Chrome DevTools から確認できます。</sub>
</p>

## CLI の使い方

### ページを抽出する

Moli 標準の完了条件を使って、ページの内容を Markdown 形式で取得します。

```bash
moli fetch \
  --dump markdown \
  --wait-until done \
  https://example.com
```

より軽量で、モデルが扱いやすいセマンティックツリー形式で直接出力することもできます。

```bash
moli fetch \
  --dump semantic_tree_text \
  --wait-selector body \
  https://example.com
```

視覚的な出力が必要な場合は、オンデマンドレイアウトを有効にすることで、ビューポートの PNG スクリーンショットやページ分割済みの PDF をそのまま生成できます。

```bash
moli fetch --layout --dump screenshot https://example.com > page.png
moli fetch --layout --dump pdf https://example.com > page.pdf
```

`fetch --help` を実行すると、出力形式、ページ読み込み・レスポンスの待機条件、プロファイル、プロキシ設定、リソースポリシー、トレースオプションなど、すべてのパラメーターを確認できます。

### 自動化サーバーを起動する

```bash
# DOM 操作中心のワークロード向けの基本自動化サーバー
moli serve

# 実際のジオメトリ計算、座標入力、スクリーンショット/スクリーンキャストを有効化
moli serve --layout

# 画像・フォント・音声・動画・メディア・テキストトラックなどのオプションリソースも取得
moli serve --layout --resource
```

同一のエンドポイントで CDP、WebDriver Classic、WebDriver BiDi の 3 プロトコルすべてに対応しています。Playwright からは CDP 経由でそのまま接続できます。

```js
import { chromium } from "playwright";

const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
const context = browser.contexts()[0];
const page = context.pages()[0] ?? await context.newPage();

await page.goto("https://example.com");
console.log(await page.locator("body").innerText());

await browser.close();
```

## Moli を選ぶ理由

エージェントのワークロードにおいて重要な 3 つの特長を、Moli はすべて兼ね備えています。

- **フル機能** — 本物の JavaScript、DOM、CSS、ネットワーク、ストレージ、レイアウト、スクリーンショット、標準の自動化プロトコルを、1 つのヘッドレスブラウザに統合しています。
- **高速** — 自動化リクエストの多くは視覚的なレンダリングを必要としないため、構造中心の操作ではレイアウトと描画をまるごとスキップします。
- **高いリソース効率** — レイアウトとピクセルは必要なときにだけ生成するため、レンダリング済みの視覚状態一式を常時保持・更新しておく必要がありません。

ブラウザ自動化タスクの多くが本当に必要としているのは、常にレンダリングされ続ける視覚世界ではなく、ページの構造情報です。Moli はネイティブな DOM とスタイルの状態を唯一の情報源として扱い、レイアウトやソフトウェア描画は、それが本当に必要な処理のときだけ実行します。

| エージェントのリクエスト | Moli の処理 |
| --- | --- |
| HTML/Markdown の抽出、DOM の照会、JS の実行、ネットワーク／ストレージの検証 | ランタイムの状態を直接読み取るだけ — レイアウトも描画も発生しない |
| 要素の境界ボックスの読み取り、座標のヒットテスト、座標入力の送信 | レイアウトを 1 回計算し、最新のジオメトリスナップショットのみ保持 |
| スクリーンショットの撮影、スクリーンキャストの更新 | 現在の DOM／スタイルから毎回再構築し、新しいフレームをレンダリングして使用後すぐに破棄 |

<p align="center">
  <a href="assets/moli_ondemand_rendering_flow.svg">
    <img
      src="assets/moli_ondemand_rendering_flow.svg"
      alt="Moli のリクエスト処理：標準では DOM を優先し、レイアウトと描画は必要なときだけ新たに構築"
      width="680"
    />
  </a>
</p>

V8、CSS、レイアウト、テキスト組版、ヒットテスト、ソフトウェア描画といった機能一式は、Moli の内部にそのまま備わっています。違うのは、視覚処理を*いつ*実行し、その結果を*どれだけの間*保持するかだけです。このコストモデルは、クローリング、ブラウザ操作エージェント、検索パイプライン、評価環境、強化学習ワークロードと特に相性が良好です。

## 現在サポートしている機能

- **フル機能の Web ランタイム** — ストリーミング HTML パース、ネイティブ DOM、V8 JavaScript、モジュール／タイマー／マイクロタスク／イベント、iframe と worker、CSS カスケード、Fetch/XHR/WebSocket、Cookie、WebCrypto、プロファイル単位のストレージ（localStorage、IndexedDB、OPFS）。
- **抽出に最適化された出力** — CLI から HTML、Markdown、JSON、セマンティックテキストツリー、フレーム情報を含むシリアライズ結果を直接出力可能。セレクターやスクリプト、レスポンスの待機条件指定、ネットワークトレースにも対応。
- **統一された自動化インターフェース** — CDP、WebDriver Classic、WebDriver BiDi は同一のカーネルとスケジューラーを共有。ChromeDriver や geckodriver、ブラウザ本体を別途インストールする必要はありません。
- **視覚機能はオンデマンドで有効化** — `--layout` を付けると、完全なボックス構築、Taffy レイアウト、Parley によるテキスト組版、レイアウトベースのヒットテスト／入力、ビューポートのスクリーンショット、CPU で低頻度にレンダリングする DevTools スクリーンキャストが使えるようになります。
- **細かく制御できる運用オプション** — プロファイル、Cookie、HTTP キャッシュ、プロキシ、リソース種別、接続数の上限、タイムアウト、プライベートネットワークポリシー、User-Agent の上書き、構造化ログ、ネットワーク診断まで一通り揃っています。

## Moli と Lexmount の関係

Moli は Lexmount が開発するオープンソースのヘッドレスブラウザで、Lexmount Browser はこれを中核に据えたマネージドクラウドランタイム兼コントロールプレーンです。

**Lexmount Browser がなくても、このオープンソース版だけですべての機能を利用できます。**

## コスト制御

Moli では、負荷の高いブラウザ処理はすべて明示的な有効化が必要で、デフォルトではオフになっています。

| モードまたはオプション | 動作 |
| --- | --- |
| 標準 | `LayoutPolicy::Mock` — 決定論的かつ形式的に互換性のあるジオメトリを返すのみで、実際のレイアウトや描画は行いません |
| `--layout` | `LayoutPolicy::OnDemand` — 実際のレイアウト計算、ジオメトリ、ヒットテスト、座標入力、スクリーンショット、スクリーンキャストを提供します |
| `--resource` | オプションの視覚・メディアリソースをすべて取得します |
| `--image`、`--font`、`--audio`、`--video`、`--media`、`--text-track` | 指定した種類のオプションリソースのみを有効化します |
| `--profile-dir`、`--http-cache-dir`、`--cookie-file` | ワークロードに応じて、必要な永続化機能だけを選択的に有効化します |

レイアウトの結果は、常時保持され続ける状態ではなく、オンデマンドで取得するスナップショットです。最初のジオメトリ要求（コールドスタート）時に、現在の DOM／スタイルから完全なレイアウトを 1 回だけ構築し、最新の `LayoutPassOutput` のみを保持します。以降はページに変化があっても、通常のジオメトリ読み取りはこのスナップショットを再利用することがあります。一方、スクリーンショットとスクリーンキャストは呼び出すたびに再構築され、古い結果が再利用されることはありません。

## アーキテクチャ

Moli は Chromium のラッパーではなく、独立したブラウザカーネルです。Rust をベースに構築されており、独自の所有権モデルとライフサイクルルールを持っています。主な依存技術は以下のとおりです。

- `libcurl` — ネットワーク転送と複数リクエストのランタイム
- `html5ever` — HTML パース
- `rusty_v8` / V8 — JavaScript 実行
- Servo/Stylo — セレクター、カスケード、計算済みスタイル
- Taffy + Parley — ボックスとテキストのレイアウト
- AnyRender/Vello CPU、`usvg`、Rust の画像エコシステム — ソフトウェアレンダリング

唯一の情報源（single source of truth）となるのは、ネイティブ DOM と Stylo が統合されたドキュメント／スタイルの状態です。実際に更新が必要になるたびに、この情報源からレイアウトを再構築し、DOM に依存しない不変（immutable）なデータへ変換したうえで、レイアウトと描画の過程で生じた一時的な状態は破棄します。システム全体を見ても、増分レイアウトツリー、ダメージグラフ、保持型のディスプレイリスト、GPU コンポジター、永続的なウィンドウといった仕組みは存在しません。

## テストデータ

以下の 2 種類の実測データは、Moli の現時点での能力範囲を示すものです。テストの対象は、実在の Web サイト、実際の自動化クライアント、Chromium/WPT の挙動を的を絞って検証したもの、そして大規模な nextest 回帰テストスイートです。

### 公開 Web の混合クロールテスト

対象は、中国国内および世界の主要サイトから選んだ 192 件の公開 URL です。成功の判定基準は、JavaScript 実行後に実質的なコンテンツが生成されることとしました。HTTP ステータス 200 が返るだけのケース、検証用のチャレンジページ、ログインウォール、空のレスポンス、外枠だけのアプリ画面は、いずれも成功として数えていません。

| ブラウザ | 有用なページ | 成功率 | 中央値の時間 | RSS 中央値 |
| --- | ---: | ---: | ---: | ---: |
| **Moli** | **103** | **53.6%** | **1.43 s** | **73 MiB** |
| Chrome Headless | 101 | 52.6% | 1.43 s | 773 MiB |
| Lightpanda | 85 | 44.3% | 0.97 s | 40 MiB |
| Obscura | 57 | 29.7% | 1.30 s | 39 MiB |

### エージェントワークロードのサンプル

| 指標 | Moli | Chromium |
| --- | ---: | ---: |
| CDP 準備完了 | 34.85 ms | 169.37 ms |
| エピソード稼働時間 p50 | 33.40 ms | 57.13 ms |
| ピーク PSS | 102.46 MiB | 348.82 MiB |
| 最大プロセス数／スレッド数 | 1 / 24 | 11 / 123 |

Moli のエージェントブラウザ機能の範囲を検証する WPT テスト群では、1 回のフルテスト実行で **161 万 2,000 件のテストが成功**しています。

## プロジェクトの対象範囲

ドキュメントで定義しているエージェントブラウザとしての利用シーンにおいて、Moli はすでに本番投入可能な水準に達しており、現在も開発を継続しています。

現時点で意図的にスコープ外としている点は、以下のとおりです。

- GUI ブラウザ、永続ウィンドウ、GPU コンポジターは提供しておらず、保持型のマルチフレーム描画アーキテクチャも実装していません。
- Chrome とピクセル単位で一致するレンダリングは目指しておらず、高忠実度の Canvas/WebGL・メディア再生機能も提供していません。
- CDP、WebDriver Classic、WebDriver BiDi は、それぞれ一部の機能のみを対象としており、プロトコル全体の完全な互換性は実装していません。
- `--layout` モードでは、ソフトウェアによるスクリーンショットとラスター方式の CDP PDF 生成に対応していますが、Chrome が持つすべてのスクリーンショット・印刷モードを実装しているわけではありません。
- リソースの読み込み、ジオメトリの鮮度、視覚レンダリングにかかるコストは、常にデフォルトで有効になっているわけではなく、いずれも明示的なポリシーとして設定する仕様です。

未対応のプロトコル経路に対しては、明確なエラーを返します。Moli が、実行していないブラウザ操作やイベント、ネットワーク観測、視覚的な結果をあたかも実行済みであるかのように見せることはありません。

メンテナーは [リリースガイド](RELEASING.md) に従うことで、GitHub Actions 経由でタグ付きのバイナリリリースを公開できます。

## ライセンス

特に明記がない限り、Moli は [Apache License 2.0](LICENSE-APACHE) または [MIT License](LICENSE-MIT) のいずれかを選んで利用できます（デュアルライセンス）。個別のライセンスが定められているサードパーティ製コンポーネントやフィクスチャについては、それぞれのライセンスおよび告知に従います。