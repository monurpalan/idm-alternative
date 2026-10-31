# Privacy

*Türkçesi aşağıda.*

Shardload has no accounts, no ads, no analytics and no telemetry. Nothing you download or
browse is sent to the developer or to anyone else.

## The browser extension

The extension only talks to the Shardload app installed on the same computer, through the
browser's native messaging. It has no server of its own and makes no network requests.

What it reads, and why:

- **Downloads the browser starts** (address, file name, size, the page they came from): to
  hand them to the app instead of the browser. A download the app cannot take is given back to
  the browser.
- **Cookies of the site a download comes from**: sent to the app with that download only, so
  downloads that need a login work. Cookies of other sites are not read.
- **Whether a page sent a form (POST)** to an address: such downloads stay in the browser.
- **Video and audio the page loads** (their addresses and types): for the Download button over
  videos and the list in the toolbar popup. Kept in the browser's session storage per tab and
  dropped when the tab navigates or closes.
- **Links on the page**, only when you choose "Download all links with Shardload".
- **The Alt key**, held while clicking: that download stays in the browser.

Its settings (on/off, file types, minimum size, excluded sites) are kept in the browser's own
storage, which the browser may sync with your browser account. When it starts, the extension
tells the app its version and which browser it runs in, so the app can say when it needs an
update.

## The app

The app connects to:

- the servers you download from (and, to find a published checksum, the same server or the
  GitHub release a file belongs to);
- the peers and trackers of torrents you add;
- GitHub, once a day, to check for a new version of Shardload, and to download and update the
  tools it uses for video (yt-dlp, Deno, ffmpeg) when a download needs them.

Your download list, history, settings and what it learned about sites stay in the app's data
folder on your computer. Cookies and passwords in the list, and the proxy password in the
settings, are encrypted with Windows' DPAPI (on Linux they are kept as they are, in your
user's data folder). Finished files get the same "downloaded from the Internet" mark a browser adds (the
address they came from), which you can turn off in the settings.

The local MCP server, when an AI assistant on your computer starts it, can only reach the app
on the same computer.

Questions: [open an issue](https://github.com/monurpalan/idm-alternative/issues).

---

# Gizlilik

Shardload'da hesap, reklam, istatistik toplama ya da telemetri yok. İndirdikleriniz ve gezdiğiniz
sayfalar geliştiriciye ya da başka birine gönderilmez.

## Tarayıcı eklentisi

Eklenti yalnız aynı bilgisayarda kurulu Shardload uygulamasıyla, tarayıcının yerel mesajlaşması
(native messaging) üzerinden konuşur. Kendine ait bir sunucusu yoktur, ağ isteği yapmaz.

Neyi okur, niçin:

- **Tarayıcının başlattığı indirmeler** (adres, dosya adı, boyut, geldiği sayfa): tarayıcı
  yerine uygulamaya vermek için. Uygulamanın alamadığı indirme tarayıcıya geri verilir.
- **İndirmenin geldiği sitenin çerezleri**: yalnız o indirmeyle uygulamaya gider; giriş isteyen
  indirmeler böyle çalışır. Başka sitelerin çerezleri okunmaz.
- **Bir sayfanın bir adrese form (POST) gönderip göndermediği**: böyle indirmeler tarayıcıda
  kalır.
- **Sayfanın yüklediği video ve sesler** (adresleri ve türleri): videoların üstündeki İndir
  düğmesi ve araç çubuğundaki liste için. Sekme başına tarayıcının oturum belleğinde tutulur,
  sekme başka sayfaya geçince ya da kapanınca silinir.
- **Sayfadaki linkler**, yalnız "Download all links with Shardload"u seçtiğinizde.
- **Alt tuşu**, tıklarken basılıysa: o indirme tarayıcıda kalır.

Ayarları (açık/kapalı, dosya türleri, en küçük boyut, hariç siteler) tarayıcının kendi
belleğinde durur; tarayıcı bunları tarayıcı hesabınızla eşitleyebilir. Eklenti açılırken
uygulamaya sürümünü ve hangi tarayıcıda çalıştığını söyler; uygulama güncellenmesi gerektiğinde
bunu böyle bilir.

## Uygulama

Uygulama şuralara bağlanır:

- indirdiğiniz sunuculara (ve yayımlanmış bir sağlama toplamını bulmak için aynı sunucuya ya da
  dosyanın ait olduğu GitHub sürümüne);
- eklediğiniz torrent'lerin eşlerine ve izleyicilerine;
- günde bir kez yeni Shardload sürümü için GitHub'a; bir indirme gerektirdiğinde videoda
  kullandığı araçları (yt-dlp, Deno, ffmpeg) indirmek ve güncellemek için yine GitHub'a.

İndirme listesi, geçmiş, ayarlar ve sitelerden öğrendikleri bilgisayarınızdaki veri klasöründe
kalır. Listedeki çerezler ve şifreler ile ayarlardaki proxy şifresi Windows'un DPAPI'siyle
şifrelenir (Linux'ta şifrelenmeden, kullanıcınızın veri klasöründe durur). Biten dosyalara
tarayıcıların koyduğu "İnternet'ten indirildi" işareti (geldiği adres) konur; ayarlardan
kapatılabilir.

Yerel MCP sunucusu, bilgisayarınızdaki bir yapay zekâ asistanı onu başlattığında, yalnız aynı
bilgisayardaki uygulamaya ulaşabilir.

Sorular için: [bir issue açın](https://github.com/monurpalan/idm-alternative/issues).
