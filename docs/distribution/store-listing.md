# Store listings for the browser extension

Texts for the Chrome Web Store, Microsoft Edge Add-ons and Firefox Add-ons (AMO), in English and
Turkish. The package is `shardload-extension-chromium.zip` (Chrome, Edge) or
`shardload-extension-firefox.zip` (Firefox) from the release. Screenshots (1280×800) are in
[screenshots/](screenshots). Privacy policy URL:
<https://github.com/monurpalan/idm-alternative/blob/main/docs/PRIVACY.md>.

Before a store version: the Chromium build's manifest carries a `key` (it fixes the extension ID
for unpacked installs). The Chrome Web Store refuses an upload with a `key`; remove it from the
uploaded `manifest.json`. The store then gives the extension its own ID, which the app's native
messaging manifest (`allowed_origins`, `CHROMIUM_EXTENSION_ID` in `crates/bridge`) must list as
well. The same holds for Edge.

## Name

Shardload

## Summary (at most 132 characters)

- EN: Hands your downloads to Shardload, the free and open-source IDM alternative: faster
  segmented downloads, video and torrents.
- TR: İndirmelerinizi Shardload'a verir: ücretsiz, açık kaynak IDM alternatifi. Parçalı hızlı
  indirme, video ve torrent.

## Category

Productivity (Chrome, Edge) · Download Management (Firefox)

## Description (EN)

Shardload is a free, open-source download manager for Windows and Linux, an alternative to
Internet Download Manager. This extension connects your browser to it.

**The Shardload app must be installed:** https://github.com/monurpalan/idm-alternative

What the extension does:

• Takes over the files you download in the browser and hands them to Shardload, with the
  site's cookies, so downloads behind a login work too. If the app cannot take one, the browser
  downloads it as usual.
• Hold Alt while clicking to let the browser download a file itself.
• Form (POST) downloads, such as "export as CSV" buttons, stay in the browser.
• A Download button over videos on the page; the toolbar button lists the videos and streams
  the page played (HLS, DASH, MP4) and sends the one you pick to the app.
• Right-click a link: "Download with Shardload". Right-click a page: "Download all links with
  Shardload" to pick from every file it links to.
• Choose which file types it takes, a minimum size, and sites to leave alone.

What Shardload does with them: splits each file into parts and downloads them at once, resumes
after a crash, checks the file against the publisher's checksum; saves videos from YouTube and
many other sites (playlists, clips, live streams), torrents and magnet links. No ads, no trial,
no account.

Privacy: the extension talks only to the app on your computer. Nothing is sent to the developer
or anyone else. https://github.com/monurpalan/idm-alternative/blob/main/docs/PRIVACY.md

## Açıklama (TR)

Shardload, Windows ve Linux için ücretsiz, açık kaynak bir indirme yöneticisi; Internet Download
Manager'a bir alternatif. Bu eklenti tarayıcınızı ona bağlar.

**Shardload uygulaması kurulu olmalı:** https://github.com/monurpalan/idm-alternative

Eklentinin yaptıkları:

• Tarayıcıda indirdiğiniz dosyaları devralıp sitenin çerezleriyle Shardload'a verir; giriş
  isteyen indirmeler de çalışır. Uygulama bir indirmeyi alamazsa tarayıcı onu her zamanki gibi
  indirir.
• Tıklarken Alt'a basılı tutarsanız dosyayı tarayıcı kendisi indirir.
• Form (POST) ile başlayan indirmeler ("CSV olarak dışa aktar" gibi düğmeler) tarayıcıda kalır.
• Sayfadaki videoların üstünde bir İndir düğmesi; araç çubuğu düğmesi sayfanın oynattığı video ve
  yayınları (HLS, DASH, MP4) listeler, seçtiğinizi uygulamaya gönderir.
• Bir linke sağ tık: "Download with Shardload". Sayfaya sağ tık: "Download all links with
  Shardload" ile sayfanın bağlantı verdiği tüm dosyalardan seçmek.
• Hangi dosya türlerini alacağını, en küçük boyutu ve dokunmayacağı siteleri siz seçersiniz.

Shardload bunlarla ne yapar: her dosyayı parçalara bölüp aynı anda indirir, çökmeden sonra
kaldığı yerden sürer, dosyayı yayıncının sağlama toplamıyla doğrular; YouTube ve pek çok siteden
video (oynatma listesi, klip, canlı yayın), torrent ve magnet linkleri indirir. Reklam, deneme
süresi ya da hesap yok.

Gizlilik: eklenti yalnız bilgisayarınızdaki uygulamayla konuşur. Geliştiriciye ya da başka
birine hiçbir şey gönderilmez.
https://github.com/monurpalan/idm-alternative/blob/main/docs/PRIVACY.md

## Single purpose (Chrome Web Store)

Hand the downloads and videos of the browser to the Shardload download manager installed on the
same computer.

## Permission justifications

| Permission | Why |
|---|---|
| `downloads` | To see the downloads the browser starts, cancel the browser's copy of the ones handed to Shardload, and download one again in the browser when the app cannot take it. |
| `nativeMessaging` | To talk to the Shardload app installed on the same computer; this is how downloads reach it. |
| `cookies` | To send the cookies of the site a download comes from, with that download only, to the local app, so downloads that need a login work. |
| `contextMenus` | For "Download with Shardload" on links and "Download all links with Shardload" on pages. |
| `scripting` | To read the links of the current page when the user chooses "Download all links with Shardload". |
| `storage` | For the extension's settings, and per tab the list of videos the page played (session storage, cleared with the tab). |
| `webRequest` | To notice form (POST) requests, whose downloads stay in the browser, and the video and audio responses a page loads, for the Download button and the toolbar list. Requests are only observed, never blocked or changed. |
| Host permission `<all_urls>` | Downloads and videos can come from any site; the content scripts (the Alt key, the Download button over videos) run on every page. |

Remote code: none. All code is in the package.

## Data usage (Chrome Web Store form)

Handled, and only passed to the Shardload app on the user's own computer:

- Authentication information: cookies of the site a download comes from.
- Web history: the address of the page a download or video came from.
- Website content: links of a page (when asked) and the addresses of its videos.

Certify: not sold to third parties; not used or transferred for purposes unrelated to the single
purpose; not used or transferred to determine creditworthiness or for lending.

## Notes for reviewers

The extension needs the Shardload app (Windows or Linux) from
https://github.com/monurpalan/idm-alternative/releases/latest. Install it, open a page with a
link to a `.zip` file, and click the link: the download opens in Shardload. Without the app the
browser downloads the file itself. The source of the extension is in the same repository, in
`extension/`; `npm ci && npm run build` builds `dist/chromium` and `dist/firefox`.

## Firefox (AMO)

- Source code: AMO asks for it because the code is bundled with esbuild. Upload a zip of the
  repository's `extension/` folder (without `node_modules` and `dist`) with the build steps
  above (Node 24).
- The add-on ID is `shardload@monurpalan.github.io` (fixed in the manifest); the app's Firefox
  native messaging manifest already allows it.
